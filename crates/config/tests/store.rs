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
///////////////////////////////////////////////////////////////////////////////

//! Host tests of the transactional CBOR configuration store.

use minicbor::{CborLen, Decode, Encode, Encoder};
use rohi_config::{
    COMMIT_WORD, ConfigError, ConfigOpenState, ConfigSection, ConfigStore, Corruption, Crc32,
    DecodeKind, FORMAT_VERSION, MAX_SECTIONS, MemoryError, MemoryStorage, OpenFailure,
    SectionError, SectionId, SectionInfo,
};

type Mem = MemoryStorage<16384, 4096>;
/// Fault injected into the storage before an operation.
type Fault = Box<dyn Fn(&mut Mem)>;
const SLOT: usize = 8192;

#[derive(Debug, Clone, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(version = 1)]
struct Identity {
    #[n(0)]
    serial: u64,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(version = 1)]
struct WifiConfig {
    #[n(0)]
    tag: u8,
    #[n(1)]
    password: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(version = 1)]
struct ConnectivityConfig {
    #[n(0)]
    interval_s: u32,
    #[n(1)]
    enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(id = 0xAABBCCDDEEFF0011, version = 1)]
struct ExplicitId {
    #[n(0)]
    value: u8,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(id = 0x1122334455667788, version = 1)]
struct Golden {
    #[n(0)]
    value: u32,
    #[n(1)]
    flag: bool,
}

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(id = 0x4000, version = 1)]
struct Narrow {
    #[n(0)]
    a: u8,
}

// Same section ID as `Narrow` with an additional field: CBOR field numbers let
// an older reader tolerate it, the version policy is still explicit.
#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(id = 0x4000, version = 2)]
struct Wide {
    #[n(0)]
    a: u8,
    #[n(1)]
    b: u8,
}

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(id = 0x1000, version = 1)]
struct MigratedV1 {
    #[n(0)]
    a: u8,
}

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(id = 0x1000, version = 2, decode_versioned = "migrate")]
struct MigratedV2 {
    #[n(0)]
    a: u8,
    #[n(1)]
    b: u8,
}

fn migrate(version: u16, payload: &[u8]) -> Result<MigratedV2, SectionError> {
    match version {
        1 => {
            let old: MigratedV1 = rohi_config::decode_exact(payload)?;
            Ok(MigratedV2 { a: old.a, b: 99 })
        }
        2 => rohi_config::decode_exact(payload),
        version => Err(SectionError::UnsupportedSectionVersion {
            id: MigratedV2::ID,
            version,
        }),
    }
}

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(id = 0x2000, version = 1)]
struct Large {
    #[cbor(n(0), with = "minicbor::bytes")]
    data: [u8; 1000],
}

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(id = 0x2001, version = 1)]
struct TooLarge {
    #[cbor(n(0), with = "minicbor::bytes")]
    data: [u8; 1021],
}

fn identity(serial: u64) -> Identity {
    Identity { serial }
}

fn wifi(tag: u8) -> WifiConfig {
    WifiConfig {
        tag,
        password: Some(u32::from(tag) * 1000),
    }
}

fn connectivity(interval_s: u32) -> ConnectivityConfig {
    ConnectivityConfig {
        interval_s,
        enabled: true,
    }
}

fn open(mem: Mem) -> ConfigStore<Mem> {
    ConfigStore::open(mem).unwrap()
}

fn corrupt_reason(mem: Mem) -> Corruption {
    match ConfigStore::open(mem) {
        Err(ConfigError::CorruptStorage(reason)) => reason,
        Ok(store) => panic!("opened as {:?}", store.open_state()),
        Err(error) => panic!("unexpected error {error:?}"),
    }
}

// Builders of persistent CBOR documents, independent of the store's writer.

fn record(id: u64, version: u16, payload: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.array(3)
        .unwrap()
        .u64(id)
        .unwrap()
        .u16(version)
        .unwrap()
        .bytes(payload)
        .unwrap();
    e.into_writer()
}

fn config_payload(format: u16, generation: u32, records: &[Vec<u8>]) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.map(3)
        .unwrap()
        .u8(0)
        .unwrap()
        .u16(format)
        .unwrap()
        .u8(1)
        .unwrap()
        .u32(generation)
        .unwrap()
        .u8(2)
        .unwrap()
        .array(records.len() as u64)
        .unwrap();
    for record in records {
        e.bytes(record).unwrap();
    }
    e.into_writer()
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finish()
}

fn envelope_with_crc(payload: &[u8], crc: u32) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.map(2)
        .unwrap()
        .u8(0)
        .unwrap()
        .bytes(payload)
        .unwrap()
        .u8(1)
        .unwrap()
        .u32(crc)
        .unwrap();
    e.into_writer()
}

fn envelope(payload: &[u8]) -> Vec<u8> {
    envelope_with_crc(payload, crc32(payload))
}

/// A complete slot image: envelope, erased padding and optionally the commit word.
fn slot_image(envelope: &[u8], committed: bool) -> Vec<u8> {
    let mut image = vec![0xFF; SLOT];
    image[..envelope.len()].copy_from_slice(envelope);
    if committed {
        image[SLOT - 4..].copy_from_slice(&COMMIT_WORD.to_le_bytes());
    }
    image
}

fn valid_image(generation: u32, records: &[Vec<u8>]) -> Vec<u8> {
    slot_image(
        &envelope(&config_payload(FORMAT_VERSION, generation, records)),
        true,
    )
}

fn install(mem: &mut Mem, slot: usize, image: &[u8]) {
    assert_eq!(image.len(), SLOT);
    mem.as_bytes_mut()[slot * SLOT..(slot + 1) * SLOT].copy_from_slice(image);
}

fn conn_record(interval: u32) -> Vec<u8> {
    let mut payload = [0u8; 16];
    let len = rohi_config::encode_to_slice(&connectivity(interval), &mut payload).unwrap();
    record(ConnectivityConfig::ID.0, 1, &payload[..len])
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn clone(mem: &Mem) -> Mem {
    let mut copy = Mem::new();
    copy.as_bytes_mut().copy_from_slice(mem.as_bytes());
    copy
}

#[test]
fn empty_storage_opens_empty() {
    let mut store = open(Mem::new());
    assert_eq!(store.open_state(), ConfigOpenState::Empty);
    assert_eq!(store.generation(), None);
    assert_eq!(store.active_slot(), None);
    assert_eq!(store.section_count(), 0);
    assert_eq!(store.get::<Identity>().unwrap(), None);
    assert!(!store.contains::<WifiConfig>().unwrap());
}

#[test]
fn typed_round_trip_survives_reopen() {
    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    assert_eq!(store.generation(), Some(1));
    assert_eq!(store.active_slot(), Some(0));
    let mut store = open(store.into_storage());
    assert_eq!(store.open_state(), ConfigOpenState::Valid);
    assert_eq!(store.get::<WifiConfig>().unwrap(), Some(wifi(1)));
    assert_eq!(store.get::<ConnectivityConfig>().unwrap(), None);
    store.set(&wifi(2)).unwrap();
    assert_eq!(store.generation(), Some(2));
    assert_eq!(store.active_slot(), Some(1));
}

#[test]
fn multiple_sections_are_independent() {
    let mut store = open(Mem::new());
    store.set(&Identity { serial: 7 }).unwrap();
    store.set(&wifi(3)).unwrap();
    store.set(&connectivity(60)).unwrap();
    let mut store = open(store.into_storage());
    assert_eq!(store.section_count(), 3);
    assert_eq!(
        store.get::<Identity>().unwrap(),
        Some(Identity { serial: 7 })
    );
    assert_eq!(store.get::<WifiConfig>().unwrap(), Some(wifi(3)));
    assert_eq!(
        store.get::<ConnectivityConfig>().unwrap(),
        Some(connectivity(60))
    );
    let mut infos = Vec::new();
    store.for_each_section(|info| infos.push(info)).unwrap();
    assert_eq!(
        infos.iter().map(|i| i.id).collect::<Vec<_>>(),
        [Identity::ID, WifiConfig::ID, ConnectivityConfig::ID]
    );
    assert!(infos.iter().all(|i| i.version == 1));
}

// Changing any of these values is a breaking persistent storage change.
#[test]
fn section_id_golden_values() {
    assert_eq!(Identity::ID, SectionId(0x94d6_3573_d312_fe9b));
    assert_eq!(WifiConfig::ID, SectionId(0x3352_46ad_2024_8b26));
    assert_eq!(ConnectivityConfig::ID, SectionId(0x9c57_015a_4066_2d0c));
    assert_eq!(Identity::ID, SectionId::from_name("Identity"));
    assert_eq!(ExplicitId::ID, SectionId(0xAABB_CCDD_EEFF_0011));
    assert_eq!(WifiConfig::VERSION, 1);
}

#[test]
fn replacement_preserves_unrelated_sections_and_order() {
    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    store.set(&connectivity(1)).unwrap();
    store.set(&wifi(2)).unwrap();
    let mut store = open(store.into_storage());
    let mut order = Vec::new();
    store.for_each_section(|info| order.push(info.id)).unwrap();
    assert_eq!(order, [WifiConfig::ID, ConnectivityConfig::ID]);
    assert_eq!(store.get::<WifiConfig>().unwrap(), Some(wifi(2)));
    assert_eq!(
        store.get::<ConnectivityConfig>().unwrap(),
        Some(connectivity(1))
    );
}

#[test]
fn remove_and_clear() {
    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    store.set(&connectivity(1)).unwrap();
    store.remove::<WifiConfig>().unwrap();
    assert_eq!(store.get::<WifiConfig>().unwrap(), None);
    assert_eq!(
        store.get::<ConnectivityConfig>().unwrap(),
        Some(connectivity(1))
    );
    let generation = store.generation();
    // Removing an absent section commits nothing.
    store.remove::<WifiConfig>().unwrap();
    assert_eq!(store.generation(), generation);
    store.clear().unwrap();
    assert_eq!(store.section_count(), 0);
    let mut store = open(store.into_storage());
    assert_eq!(store.open_state(), ConfigOpenState::Valid);
    assert_eq!(store.get::<ConnectivityConfig>().unwrap(), None);
    // Clearing an empty configuration commits nothing either.
    let generation = store.generation();
    store.clear().unwrap();
    assert_eq!(store.generation(), generation);
}

#[test]
fn clearing_empty_storage_writes_nothing() {
    let mut store = open(Mem::new());
    store.clear().unwrap();
    assert_eq!(store.generation(), None);
    assert_eq!(store.storage().bytes_written(), 0);
}

// Golden fixtures: accidental changes of the persistent format must be visible.
#[test]
fn cbor_golden_fixtures() {
    let golden = Golden {
        value: 42,
        flag: true,
    };

    // Typed section payload: [42, true].
    let mut typed = [0u8; 16];
    let len = rohi_config::encode_to_slice(&golden, &mut typed).unwrap();
    assert_eq!(&typed[..len], [0x82, 0x18, 0x2A, 0xF5]);
    assert_eq!(minicbor::len(&golden), 4);

    // SectionRecord: [ id, version, bstr(typed) ].
    let expected_record = [
        0x83, 0x1B, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x01, 0x44, 0x82, 0x18, 0x2A,
        0xF5,
    ];
    assert_eq!(record(Golden::ID.0, 1, &typed[..len]), expected_record);

    // ConfigPayload: { 0: format, 1: generation, 2: [ bstr(record) ] }.
    let mut expected_payload = vec![0xA3, 0x00, 0x01, 0x01, 0x01, 0x02, 0x81, 0x50];
    expected_payload.extend_from_slice(&expected_record);
    assert_eq!(
        config_payload(FORMAT_VERSION, 1, &[expected_record.to_vec()]),
        expected_payload
    );

    // SlotEnvelope: { 0: bstr(payload), 1: crc32(payload) }.
    assert_eq!(crc32(&expected_payload), 0xE47B_3143);
    let mut expected_envelope = vec![0xA2, 0x00, 0x58, 0x18];
    expected_envelope.extend_from_slice(&expected_payload);
    expected_envelope.extend_from_slice(&[0x01, 0x1A, 0xE4, 0x7B, 0x31, 0x43]);

    // The store's own writer produces exactly these bytes, deterministically.
    let mut store = open(Mem::new());
    store.set(&golden).unwrap();
    let mem = store.into_storage();
    let slot = &mem.as_bytes()[..SLOT];
    assert_eq!(&slot[..expected_envelope.len()], expected_envelope);
    assert!(
        slot[expected_envelope.len()..SLOT - 4]
            .iter()
            .all(|b| *b == 0xFF)
    );
    assert_eq!(&slot[SLOT - 4..], b"COMT");
    assert!(mem.as_bytes()[SLOT..].iter().all(|b| *b == 0xFF));

    let mut again = open(Mem::new());
    again.set(&golden).unwrap();
    assert_eq!(again.storage().as_bytes(), mem.as_bytes());
    let mut store = open(mem);
    assert_eq!(store.get::<Golden>().unwrap(), Some(golden));
}

#[test]
fn encoded_size_comes_from_cbor_len() {
    let mut store = open(Mem::new());
    // array(1) + 3 byte head + 1000 bytes.
    assert_eq!(minicbor::len(&Large { data: [7; 1000] }), 1 + 3 + 1000);
    store.set(&Large { data: [7; 1000] }).unwrap();
    assert_eq!(store.get::<Large>().unwrap().unwrap().data[999], 7);
    let mut info = None;
    store.for_each_section(|i| info = Some(i)).unwrap();
    assert_eq!(
        info,
        Some(SectionInfo {
            id: Large::ID,
            version: 1,
            length: 1004
        })
    );

    let written = store.storage().bytes_written();
    let erased = store.storage().bytes_erased();
    assert!(matches!(
        store.set(&TooLarge { data: [0; 1021] }),
        Err(ConfigError::SectionTooLarge {
            len: 1025,
            max: 1024,
            ..
        })
    ));
    assert_eq!(store.storage().bytes_written(), written);
    assert_eq!(store.storage().bytes_erased(), erased);
}

#[test]
fn trailing_bytes_are_rejected_with_a_typed_error() {
    let mut store = open(Mem::new());
    // A valid `Narrow` followed by garbage.
    store.set_raw(Narrow::ID, 1, &[0x81, 0x05, 0xFF]).unwrap();
    assert_eq!(
        store.get::<Narrow>(),
        Err(ConfigError::Section(SectionError::TrailingBytes {
            extra: 1
        }))
    );
    store.set_raw(Narrow::ID, 1, &[0x81, 0x05]).unwrap();
    assert_eq!(store.get::<Narrow>().unwrap(), Some(Narrow { a: 5 }));
}

#[test]
fn undecodable_payloads_are_typed_errors() {
    let mut store = open(Mem::new());
    store.set_raw(Narrow::ID, 1, &[0x81]).unwrap();
    match store.get::<Narrow>() {
        Err(ConfigError::Section(SectionError::Decode(failure))) => {
            assert_eq!(failure.kind, DecodeKind::EndOfInput)
        }
        other => panic!("unexpected {other:?}"),
    }
    store.set_raw(Narrow::ID, 1, &[0x60]).unwrap();
    match store.get::<Narrow>() {
        Err(ConfigError::Section(SectionError::Decode(failure))) => {
            assert_eq!(failure.kind, DecodeKind::TypeMismatch)
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn unknown_cbor_fields_do_not_replace_the_version_policy() {
    let mut store = open(Mem::new());
    store.set(&Wide { a: 1, b: 2 }).unwrap();
    // The payload would decode as `Narrow`, but it was written as version 2.
    assert_eq!(
        store.get::<Narrow>(),
        Err(ConfigError::Section(
            SectionError::UnsupportedSectionVersion {
                id: Narrow::ID,
                version: 2
            }
        ))
    );
    assert_eq!(store.get::<Wide>().unwrap(), Some(Wide { a: 1, b: 2 }));
}

#[test]
fn section_version_policy() {
    let mut store = open(Mem::new());
    // Current version.
    store.set(&MigratedV2 { a: 1, b: 2 }).unwrap();
    assert_eq!(
        store.get::<MigratedV2>().unwrap(),
        Some(MigratedV2 { a: 1, b: 2 })
    );
    // Older version with a migration hook.
    store.set(&MigratedV1 { a: 5 }).unwrap();
    assert_eq!(
        store.get::<MigratedV2>().unwrap(),
        Some(MigratedV2 { a: 5, b: 99 })
    );
    // Newer version without a hook; the hook of the section also rejects it.
    store.set_raw(MigratedV2::ID, 3, &[0xA0]).unwrap();
    assert_eq!(
        store.get::<MigratedV1>(),
        Err(ConfigError::Section(
            SectionError::UnsupportedSectionVersion {
                id: MigratedV1::ID,
                version: 3
            }
        ))
    );
    assert_eq!(
        store.get::<MigratedV2>(),
        Err(ConfigError::Section(
            SectionError::UnsupportedSectionVersion {
                id: MigratedV2::ID,
                version: 3
            }
        ))
    );
}

#[test]
fn unknown_sections_survive_updates_byte_for_byte() {
    // Not valid CBOR and written by a hypothetical newer firmware: it must
    // never be decoded or reinterpreted.
    const UNKNOWN_ID: SectionId = SectionId(0xDEAD_BEEF_0000_0001);
    const UNKNOWN_PAYLOAD: [u8; 9] = [0xDE, 0xAD, 0xBE, 0xEF, 0xFF, 0x00, 0x1F, 0x7F, 0x01];

    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    store.set_raw(UNKNOWN_ID, 9, &UNKNOWN_PAYLOAD).unwrap();
    store.set(&connectivity(1)).unwrap();
    let expected = record(UNKNOWN_ID.0, 9, &UNKNOWN_PAYLOAD);
    assert!(contains(store.storage().as_bytes(), &expected));

    let mut seen = std::collections::HashSet::new();
    for round in 2..8u8 {
        store.set(&wifi(round)).unwrap();
        store.set(&connectivity(u32::from(round))).unwrap();
        store.remove::<ConnectivityConfig>().unwrap();
        let slot = store.active_slot().unwrap();
        let bytes = &store.storage().as_bytes()[slot * SLOT..(slot + 1) * SLOT];
        assert!(contains(bytes, &expected), "round {round}");
        seen.insert(slot);

        let mut raw = [0u8; 16];
        let info = store.read_raw(UNKNOWN_ID, &mut raw).unwrap().unwrap();
        assert_eq!(info.version, 9);
        assert_eq!(&raw[..info.length], UNKNOWN_PAYLOAD);
    }
    assert_eq!(seen.len(), 2, "both slots were used");

    // Only an explicit removal drops it.
    store.remove_raw(UNKNOWN_ID).unwrap();
    assert_eq!(store.read_raw(UNKNOWN_ID, &mut [0u8; 16]).unwrap(), None);
    // The stale copy may remain in the inactive slot until it is erased.
    let slot = store.active_slot().unwrap();
    assert!(!contains(
        &store.storage().as_bytes()[slot * SLOT..(slot + 1) * SLOT],
        &expected
    ));
}

#[test]
fn read_raw_reports_small_buffers() {
    let mut store = open(Mem::new());
    store.set(&connectivity(1)).unwrap();
    let mut small = [0u8; 2];
    assert_eq!(
        store.read_raw(ConnectivityConfig::ID, &mut small),
        Err(ConfigError::BufferTooSmall { required: 3 })
    );
    let mut big = [0u8; 8];
    let info = store
        .read_raw(ConnectivityConfig::ID, &mut big)
        .unwrap()
        .unwrap();
    assert_eq!(info.length, 3);
}

// Corruption of committed storage.

fn valid_records() -> Vec<Vec<u8>> {
    vec![conn_record(60)]
}

fn assert_connectivity(mem: Mem, interval: u32, state: ConfigOpenState) {
    let mut store = open(mem);
    assert_eq!(store.open_state(), state);
    assert_eq!(
        store.get::<ConnectivityConfig>().unwrap(),
        Some(connectivity(interval))
    );
}

#[test]
fn crafted_valid_slot_opens() {
    let mut mem = Mem::new();
    install(&mut mem, 0, &valid_image(5, &valid_records()));
    assert_connectivity(mem, 60, ConfigOpenState::Valid);
}

/// Every case is a committed slot that must not validate, with the reason.
fn invalid_slots() -> Vec<(&'static str, Vec<u8>, Corruption)> {
    let good_payload = config_payload(FORMAT_VERSION, 5, &valid_records());
    let good_envelope = envelope(&good_payload);
    let mut cases = Vec::new();

    // Outer CBOR.
    let mut image = slot_image(&good_envelope, true);
    image[0] = 0x80;
    cases.push(("envelope not a map", image, Corruption::Envelope));
    let mut image = slot_image(&good_envelope, true);
    image[1] = 0x05;
    cases.push(("envelope wrong key", image, Corruption::Envelope));
    let mut image = slot_image(&good_envelope, true);
    image[2] = 0x5F;
    cases.push(("envelope indefinite bytes", image, Corruption::Envelope));
    let mut image = slot_image(&good_envelope, true);
    image[2] = 0x59;
    image[3] = 0xFF;
    image[4] = 0xFF;
    cases.push(("envelope length beyond slot", image, Corruption::Envelope));
    cases.push((
        "erased slot with commit word",
        slot_image(&[], true),
        Corruption::Envelope,
    ));

    // CRC.
    let mut image = slot_image(&good_envelope, true);
    let crc_end = good_envelope.len() - 1;
    image[crc_end] ^= 0x01;
    cases.push(("crc byte flipped", image, Corruption::Crc));
    let mut image = slot_image(&good_envelope, true);
    image[10] ^= 0x40;
    cases.push(("payload byte flipped", image, Corruption::Crc));
    cases.push((
        "wrong crc",
        slot_image(&envelope_with_crc(&good_payload, 0), true),
        Corruption::Crc,
    ));

    // ConfigPayload with a valid CRC.
    let payload_cases: Vec<(&'static str, Vec<u8>, Corruption)> = vec![
        ("payload not a map", vec![0x80], Corruption::Payload),
        ("payload empty", vec![], Corruption::Payload),
        (
            "payload wrong map size",
            vec![0xA2, 0x00, 0x01, 0x01, 0x01],
            Corruption::Payload,
        ),
        (
            "payload wrong key order",
            vec![0xA3, 0x01, 0x01, 0x00, 0x01, 0x02, 0x80],
            Corruption::Payload,
        ),
        (
            "payload generation too large",
            vec![
                0xA3, 0x00, 0x01, 0x01, 0x1B, 0, 0, 0, 1, 0, 0, 0, 0, 0x02, 0x80,
            ],
            Corruption::Payload,
        ),
        (
            "payload indefinite sections",
            vec![0xA3, 0x00, 0x01, 0x01, 0x01, 0x02, 0x9F, 0xFF],
            Corruption::Payload,
        ),
        (
            "payload sections not bytes",
            vec![0xA3, 0x00, 0x01, 0x01, 0x01, 0x02, 0x81, 0x01],
            Corruption::Payload,
        ),
        (
            "payload trailing bytes",
            {
                let mut p = config_payload(FORMAT_VERSION, 5, &valid_records());
                p.push(0x00);
                p
            },
            Corruption::Payload,
        ),
        (
            "payload fewer records than announced",
            vec![0xA3, 0x00, 0x01, 0x01, 0x01, 0x02, 0x82, 0x40],
            Corruption::Record,
        ),
        (
            "unsupported format version",
            config_payload(FORMAT_VERSION + 1, 5, &valid_records()),
            Corruption::UnsupportedFormatVersion(FORMAT_VERSION + 1),
        ),
    ];
    for (name, payload, reason) in payload_cases {
        cases.push((name, slot_image(&envelope(&payload), true), reason));
    }

    // SectionRecord with a valid CRC.
    let record_cases: Vec<(&'static str, Vec<u8>)> = vec![
        ("record is a map", vec![0xA0]),
        ("record empty", vec![]),
        ("record array too short", vec![0x82, 0x01, 0x01]),
        ("record array too long", vec![0x84, 0x01, 0x01, 0x40, 0x00]),
        ("record id not a number", vec![0x83, 0x60, 0x01, 0x40]),
        (
            "record version too large",
            vec![0x83, 0x01, 0x1A, 0, 1, 0, 0, 0x40],
        ),
        ("record payload not bytes", vec![0x83, 0x01, 0x01, 0x60]),
        (
            "record payload truncated",
            vec![0x83, 0x01, 0x01, 0x45, 0x00],
        ),
        ("record trailing bytes", vec![0x83, 0x01, 0x01, 0x40, 0x00]),
        ("record payload above the section limit", {
            let mut r = vec![0x83, 0x01, 0x01, 0x59, 0x04, 0x01];
            r.extend_from_slice(&[0u8; 1025]);
            r
        }),
    ];
    for (name, bad) in record_cases {
        let payload = config_payload(FORMAT_VERSION, 5, &[bad]);
        cases.push((
            name,
            slot_image(&envelope(&payload), true),
            Corruption::Record,
        ));
    }

    // Duplicates and limits.
    let dup = config_payload(FORMAT_VERSION, 5, &[conn_record(1), conn_record(2)]);
    cases.push((
        "duplicate section id",
        slot_image(&envelope(&dup), true),
        Corruption::DuplicateSection(ConnectivityConfig::ID),
    ));
    let many: Vec<Vec<u8>> = (0..=MAX_SECTIONS as u64)
        .map(|i| record(0x7000 + i, 1, &[]))
        .collect();
    cases.push((
        "too many sections",
        slot_image(&envelope(&config_payload(FORMAT_VERSION, 5, &many)), true),
        Corruption::TooManySections,
    ));
    cases
}

#[test]
fn corrupt_committed_slot_is_never_empty() {
    for (name, image, reason) in invalid_slots() {
        for slot in 0..2 {
            let mut mem = Mem::new();
            install(&mut mem, slot, &image);
            assert_eq!(corrupt_reason(mem), reason, "{name} in slot {slot}");
        }
    }
}

#[test]
fn corrupt_committed_slot_falls_back_to_an_older_valid_one() {
    for (name, image, _) in invalid_slots() {
        for bad in 0..2 {
            let mut mem = Mem::new();
            install(&mut mem, 1 - bad, &valid_image(4, &[conn_record(30)]));
            install(&mut mem, bad, &image);
            let mut store = open(mem);
            assert_eq!(store.open_state(), ConfigOpenState::Recovered, "{name}");
            assert_eq!(store.state(), ConfigOpenState::Recovered, "{name}");
            assert_eq!(store.generation(), Some(4), "{name}");
            assert_eq!(store.active_slot(), Some(1 - bad), "{name}");
            assert_eq!(
                store.get::<ConnectivityConfig>().unwrap(),
                Some(connectivity(30)),
                "{name}"
            );
            // The next update repairs the damaged slot.
            store.set(&connectivity(31)).unwrap();
            assert_eq!(store.active_slot(), Some(bad), "{name}");
            assert_eq!(store.open_state(), ConfigOpenState::Recovered, "{name}");
            assert_eq!(store.state(), ConfigOpenState::Valid, "{name}");
            assert_connectivity(store.into_storage(), 31, ConfigOpenState::Valid);
        }
    }
}

#[test]
fn newest_committed_slot_wins_even_if_corrupt_falls_back() {
    let mut store = open(Mem::new());
    store.set(&connectivity(1)).unwrap();
    store.set(&connectivity(2)).unwrap();
    assert_eq!(store.active_slot(), Some(1));
    let mut mem = store.into_storage();
    mem.as_bytes_mut()[SLOT + 12] ^= 0xFF;
    assert_connectivity(mem, 1, ConfigOpenState::Recovered);
}

#[test]
fn damaged_older_slot_is_also_reported_as_recovered() {
    // The newest generation is intact; only the older, inactive slot is bad.
    let mut store = open(Mem::new());
    store.set(&connectivity(1)).unwrap();
    store.set(&connectivity(2)).unwrap();
    let mut mem = store.into_storage();
    mem.as_bytes_mut()[12] ^= 0xFF;
    let store = open(mem);
    assert_eq!(store.state(), ConfigOpenState::Recovered);
    assert_eq!(store.generation(), Some(2));
    assert_connectivity(store.into_storage(), 2, ConfigOpenState::Recovered);
}

#[test]
fn state_tracks_the_first_commit() {
    let mut store = open(Mem::new());
    assert_eq!(store.state(), ConfigOpenState::Empty);
    store.set(&connectivity(1)).unwrap();
    assert_eq!(store.open_state(), ConfigOpenState::Empty);
    assert_eq!(store.state(), ConfigOpenState::Valid);
}

fn open_failure<S: rohi_config::ConfigStorage>(
    result: Result<ConfigStore<S>, OpenFailure<S>>,
) -> OpenFailure<S> {
    match result {
        Ok(store) => panic!("unexpectedly opened as {:?}", store.open_state()),
        Err(failure) => failure,
    }
}

fn corrupt_both_slots() -> Mem {
    let mut store = open(Mem::new());
    store.set(&Identity { serial: 1 }).unwrap();
    store.set(&Identity { serial: 2 }).unwrap();
    let mut mem = store.into_storage();
    mem.as_bytes_mut()[8] ^= 0xFF;
    mem.as_bytes_mut()[SLOT + 8] ^= 0xFF;
    mem
}

#[test]
fn try_open_hands_the_storage_back() {
    let mem = corrupt_both_slots();
    let image = mem.as_bytes().to_vec();
    let failure = open_failure(ConfigStore::try_open(mem));
    assert!(matches!(failure.error, ConfigError::CorruptStorage(_)));
    assert_eq!(failure.storage.as_bytes(), image.as_slice());

    let failure = open_failure(ConfigStore::try_open(MemoryStorage::<64, 64>::new()));
    assert!(matches!(failure.error, ConfigError::InvalidStorage(_)));
}

#[test]
fn format_recovers_corrupt_storage() {
    let failure = open_failure(ConfigStore::try_open(corrupt_both_slots()));
    let mut store = ConfigStore::format(failure.storage).unwrap();
    assert_eq!(store.open_state(), ConfigOpenState::Empty);
    assert_eq!(store.state(), ConfigOpenState::Empty);
    assert_eq!(store.get::<Identity>().unwrap(), None);
    assert!(store.storage().as_bytes().iter().all(|b| *b == 0xFF));
    store.set(&Identity { serial: 3 }).unwrap();
    let mut store = open(store.into_storage());
    assert_eq!(store.open_state(), ConfigOpenState::Valid);
    assert_eq!(
        store.get::<Identity>().unwrap(),
        Some(Identity { serial: 3 })
    );
}

#[test]
fn format_discards_valid_and_unknown_sections() {
    let mut store = open(base_config());
    store.set_raw(SectionId(0xDEAD), 1, &[1, 2, 3]).unwrap();
    let store = ConfigStore::format(store.into_storage()).unwrap();
    let mut store = open(store.into_storage());
    assert_eq!(store.open_state(), ConfigOpenState::Empty);
    assert_eq!(store.section_count(), 0);
    assert_eq!(store.get::<WifiConfig>().unwrap(), None);
}

#[test]
fn format_rejects_invalid_geometry() {
    let failure = open_failure(ConfigStore::format(MemoryStorage::<64, 64>::new()));
    assert!(matches!(failure.error, ConfigError::InvalidStorage(_)));
}

#[test]
fn interrupted_format_is_corrupt_or_empty_and_can_be_repeated() {
    let corrupt = corrupt_both_slots();
    let mut faults: Vec<Fault> = Vec::new();
    for budget in 0..8 {
        faults.push(Box::new(move |mem: &mut Mem| mem.fail_write_after(budget)));
    }
    for budget in (0..2 * SLOT).step_by(512) {
        faults.push(Box::new(move |mem: &mut Mem| mem.fail_erase_after(budget)));
    }
    for (case, fault) in faults.iter().enumerate() {
        let mut mem = clone(&corrupt);
        fault(&mut mem);
        let mut mem = match ConfigStore::format(mem) {
            Ok(store) => store.into_storage(),
            Err(failure) => failure.storage,
        };
        mem.clear_faults();
        let mem = match ConfigStore::try_open(mem) {
            Ok(store) => {
                assert_eq!(store.open_state(), ConfigOpenState::Empty, "case {case}");
                store.into_storage()
            }
            Err(failure) => {
                assert!(
                    matches!(failure.error, ConfigError::CorruptStorage(_)),
                    "case {case}: {:?}",
                    failure.error
                );
                failure.storage
            }
        };
        let store = ConfigStore::format(mem).unwrap();
        assert_eq!(
            open(store.into_storage()).open_state(),
            ConfigOpenState::Empty
        );
    }
}

#[test]
fn both_committed_slots_corrupt_report_corrupt_storage() {
    let mut store = open(Mem::new());
    store.set(&Identity { serial: 1 }).unwrap();
    store.set(&Identity { serial: 2 }).unwrap();
    let mut mem = store.into_storage();
    mem.as_bytes_mut()[8] ^= 0xFF;
    mem.as_bytes_mut()[SLOT + 8] ^= 0xFF;
    assert!(matches!(
        ConfigStore::open(mem),
        Err(ConfigError::CorruptStorage(_))
    ));
}

#[test]
fn corrupt_identity_is_not_reported_as_absent() {
    let mut store = open(Mem::new());
    store.set(&Identity { serial: 0xABCD }).unwrap();
    let mut mem = store.into_storage();
    for index in [0usize, 5, 12, 20] {
        mem.as_bytes_mut()[index] ^= 0x55;
        match ConfigStore::open(clone(&mem)) {
            Err(ConfigError::CorruptStorage(_)) => {}
            Ok(mut store) => panic!(
                "opened as {:?}, identity {:?}",
                store.open_state(),
                store.get::<Identity>()
            ),
            Err(error) => panic!("{error:?}"),
        }
        mem.as_bytes_mut()[index] ^= 0x55;
    }
    // Genuinely erased storage is the only "not provisioned" case.
    let mut fresh = open(Mem::new());
    assert_eq!(fresh.get::<Identity>().unwrap(), None);
}

#[test]
fn uncommitted_slots_are_ignored() {
    let good = config_payload(FORMAT_VERSION, 5, &valid_records());
    // Complete document without the commit word, in both slots.
    let mut mem = Mem::new();
    install(&mut mem, 0, &slot_image(&envelope(&good), false));
    install(&mut mem, 1, &slot_image(&envelope(&good), false));
    let store = open(mem);
    assert_eq!(store.open_state(), ConfigOpenState::Empty);
    assert_eq!(store.generation(), None);

    // Torn first write: a prefix of the envelope, slot B erased.
    let envelope_bytes = envelope(&good);
    for cut in 0..envelope_bytes.len() {
        let mut mem = Mem::new();
        install(&mut mem, 0, &slot_image(&envelope_bytes[..cut], false));
        assert_eq!(open(mem).open_state(), ConfigOpenState::Empty, "cut {cut}");
    }

    // An uncommitted newer slot never shadows the committed one.
    let mut mem = Mem::new();
    install(&mut mem, 0, &valid_image(4, &[conn_record(30)]));
    install(
        &mut mem,
        1,
        &slot_image(
            &envelope(&config_payload(FORMAT_VERSION, 5, &[conn_record(99)])),
            false,
        ),
    );
    assert_connectivity(mem, 30, ConfigOpenState::Valid);

    // A torn commit word does not commit.
    let mut image = valid_image(5, &[conn_record(99)]);
    image[SLOT - 4..].copy_from_slice(&[b'C', b'O', 0xFF, 0xFF]);
    let mut mem = Mem::new();
    install(&mut mem, 0, &valid_image(4, &[conn_record(30)]));
    install(&mut mem, 1, &image);
    assert_connectivity(mem, 30, ConfigOpenState::Valid);
}

// Generations.

#[test]
fn generation_wraps_around() {
    let mut mem = Mem::new();
    install(&mut mem, 0, &valid_image(u32::MAX, &[conn_record(60)]));
    let mut store = open(mem);
    assert_eq!(store.generation(), Some(u32::MAX));
    store.set(&connectivity(61)).unwrap();
    assert_eq!(store.generation(), Some(0));
    let mut store = open(store.into_storage());
    assert_eq!(store.generation(), Some(0));
    assert_eq!(store.active_slot(), Some(1));
    assert_eq!(
        store.get::<ConnectivityConfig>().unwrap(),
        Some(connectivity(61))
    );
    store.set(&connectivity(62)).unwrap();
    assert_eq!(store.generation(), Some(1));
    let mut store = open(store.into_storage());
    assert_eq!(
        store.get::<ConnectivityConfig>().unwrap(),
        Some(connectivity(62))
    );
}

#[test]
fn newest_valid_slot_is_selected_across_the_wrap() {
    for (a, b, newest) in [
        (u32::MAX, 0, 1),
        (0, u32::MAX, 0),
        (7, 8, 1),
        (8, 7, 0),
        (u32::MAX - 1, u32::MAX, 1),
        (0, 1, 1),
    ] {
        let mut mem = Mem::new();
        install(&mut mem, 0, &valid_image(a, &[conn_record(a)]));
        install(&mut mem, 1, &valid_image(b, &[conn_record(b)]));
        let mut store = open(mem);
        assert_eq!(store.open_state(), ConfigOpenState::Valid);
        assert_eq!(store.active_slot(), Some(newest), "{a} vs {b}");
        let expected = if newest == 0 { a } else { b };
        assert_eq!(
            store.get::<ConnectivityConfig>().unwrap(),
            Some(connectivity(expected))
        );
    }
}

#[test]
fn generations_increment_and_alternate_slots() {
    let mut store = open(Mem::new());
    for round in 1..=6u32 {
        store.set(&connectivity(round)).unwrap();
        assert_eq!(store.generation(), Some(round));
        assert_eq!(store.active_slot(), Some((round as usize + 1) % 2));
    }
}

// Limits.

#[test]
fn storage_full_leaves_previous_configuration_untouched() {
    let mut store = open(Mem::new());
    store.set(&connectivity(5)).unwrap();
    let mut stored = 0u64;
    let failure = loop {
        let before = (
            store.storage().bytes_written(),
            store.storage().bytes_erased(),
        );
        let generation = store.generation();
        match store.set_raw(SectionId(0x5000 + stored), 1, &[0x42; 1000]) {
            Ok(()) => stored += 1,
            Err(error) => {
                // No partial write may begin when the result cannot fit.
                assert_eq!(
                    before,
                    (
                        store.storage().bytes_written(),
                        store.storage().bytes_erased()
                    )
                );
                assert_eq!(store.generation(), generation);
                break error;
            }
        }
        assert!(stored < 16, "slot never filled up");
    };
    assert_eq!(failure, ConfigError::StorageFull);
    assert!(stored >= 6);
    let mut store = open(store.into_storage());
    assert_eq!(store.open_state(), ConfigOpenState::Valid);
    assert_eq!(store.section_count(), stored as usize + 1);
    assert_eq!(
        store.get::<ConnectivityConfig>().unwrap(),
        Some(connectivity(5))
    );
}

#[test]
fn replacing_a_section_that_no_longer_fits_fails_cleanly() {
    let mut store = open(Mem::new());
    for i in 0..7u64 {
        store.set_raw(SectionId(0x5000 + i), 1, &[1; 1024]).unwrap();
    }
    store.set_raw(SectionId(0x6000), 1, &[2; 10]).unwrap();
    let generation = store.generation();
    assert_eq!(
        store.set_raw(SectionId(0x6000), 1, &[2; 1024]),
        Err(ConfigError::StorageFull)
    );
    assert_eq!(store.generation(), generation);
    let mut raw = [0u8; 1024];
    let info = store
        .read_raw(SectionId(0x6000), &mut raw)
        .unwrap()
        .unwrap();
    assert_eq!(info.length, 10);
}

#[test]
fn section_count_limit_is_enforced() {
    let mut store = open(Mem::new());
    for i in 0..MAX_SECTIONS as u64 {
        store.set_raw(SectionId(0x3000 + i), 1, &[0xF6]).unwrap();
    }
    assert_eq!(store.section_count(), MAX_SECTIONS);
    let generation = store.generation();
    assert_eq!(
        store.set_raw(SectionId(0x3FFF), 1, &[0xF6]),
        Err(ConfigError::TooManySections)
    );
    assert_eq!(store.generation(), generation);
    // Replacing an existing section stays possible.
    store.set_raw(SectionId(0x3000), 1, &[0xF5]).unwrap();
    let mut store = open(store.into_storage());
    assert_eq!(store.section_count(), MAX_SECTIONS);
    let mut raw = [0u8; 4];
    store.read_raw(SectionId(0x3000), &mut raw).unwrap();
    assert_eq!(raw[0], 0xF5);
}

#[test]
fn raw_section_size_limit() {
    let mut store = open(Mem::new());
    assert!(matches!(
        store.set_raw(SectionId(1), 1, &[0; 1025]),
        Err(ConfigError::SectionTooLarge {
            len: 1025,
            max: 1024,
            ..
        })
    ));
    store.set_raw(SectionId(1), 1, &[0; 1024]).unwrap();
}

#[test]
fn storage_geometry_is_validated() {
    assert!(matches!(
        ConfigStore::open(MemoryStorage::<64, 32>::new()),
        Err(ConfigError::InvalidStorage(_))
    ));
    assert!(matches!(
        ConfigStore::open(MemoryStorage::<4096, 4096>::new()),
        Err(ConfigError::InvalidStorage(_))
    ));
    ConfigStore::open(MemoryStorage::<256, 128>::new()).unwrap();
}

#[test]
fn small_slots_report_storage_full() {
    let mut store = ConfigStore::open(MemoryStorage::<256, 128>::new()).unwrap();
    store.set(&connectivity(1)).unwrap();
    assert_eq!(
        store.set_raw(SectionId(2), 1, &[0; 200]),
        Err(ConfigError::StorageFull)
    );
    assert_eq!(
        store.get::<ConnectivityConfig>().unwrap(),
        Some(connectivity(1))
    );
}

// Power loss.

fn state(mem: Mem) -> (Mem, Option<WifiConfig>, Option<ConnectivityConfig>) {
    let mut store = ConfigStore::open(mem)
        .unwrap_or_else(|error| panic!("interrupted update corrupted the store: {error:?}"));
    let w = store.get::<WifiConfig>().unwrap();
    let c = store.get::<ConnectivityConfig>().unwrap();
    (store.into_storage(), w, c)
}

fn base_config() -> Mem {
    let mut base = open(Mem::new());
    base.set(&wifi(1)).unwrap();
    base.set(&connectivity(60)).unwrap();
    base.into_storage()
}

#[test]
fn interrupted_write_never_loses_the_previous_configuration() {
    let base = base_config();
    let mut probe = open(clone(&base));
    let before = probe.storage().bytes_written();
    probe.set(&wifi(2)).unwrap();
    let total = probe.storage().bytes_written() - before;
    assert!(total > 0);

    for budget in 0..=total + 4 {
        let mut mem = clone(&base);
        mem.fail_write_after(budget);
        let mut store = open(mem);
        let result = store.set(&wifi(2));
        let mut mem = store.into_storage();
        mem.clear_faults();
        let (_, w, c) = state(mem);
        assert_eq!(c, Some(connectivity(60)), "budget {budget}");
        match result {
            Ok(()) => assert_eq!(w, Some(wifi(2)), "budget {budget}"),
            // The commit word is the last byte programmed: no failed update
            // can have become active, a partially written one never is.
            Err(_) => assert_eq!(w, Some(wifi(1)), "budget {budget}"),
        }
    }
}

#[test]
fn only_the_commit_word_makes_an_update_visible() {
    let base = base_config();
    let mut probe = open(clone(&base));
    let before = probe.storage().bytes_written();
    probe.set(&wifi(2)).unwrap();
    let total = probe.storage().bytes_written() - before;
    // Every budget short of the full update, including a torn commit word.
    for budget in 0..total {
        let mut mem = clone(&base);
        mem.fail_write_after(budget);
        let mut store = open(mem);
        assert!(store.set(&wifi(2)).is_err(), "budget {budget}");
        let mut mem = store.into_storage();
        mem.clear_faults();
        assert_eq!(state(mem).1, Some(wifi(1)), "budget {budget}");
    }
}

#[test]
fn interrupted_erase_never_loses_the_previous_configuration() {
    let base = base_config();
    for budget in (0..=SLOT).step_by(256) {
        let mut mem = clone(&base);
        mem.fail_erase_after(budget);
        let mut store = open(mem);
        let result = store.set(&wifi(2));
        let mut mem = store.into_storage();
        mem.clear_faults();
        let (mem, w, c) = state(mem);
        assert_eq!(c, Some(connectivity(60)), "budget {budget}");
        match result {
            Ok(()) => assert_eq!(w, Some(wifi(2)), "budget {budget}"),
            Err(_) => assert_eq!(w, Some(wifi(1)), "budget {budget}"),
        }
        // The store keeps working after the interruption.
        let mut store = open(mem);
        store.set(&wifi(3)).unwrap();
        assert_eq!(
            open(store.into_storage()).get::<WifiConfig>().unwrap(),
            Some(wifi(3))
        );
    }
}

/// Base configuration plus raw sections large enough for the document to
/// reach into the last erase sector of its slot.
fn large_base_config() -> Mem {
    let mut store = open(base_config());
    for id in 0..5u64 {
        store
            .set_raw(SectionId(0x7000 + id), 1, &[id as u8; 1000])
            .unwrap();
    }
    // Rewrite once more so both slots hold a large committed document.
    store.set(&connectivity(60)).unwrap();
    let mem = store.into_storage();
    for slot in 0..2 {
        let last_sector = &mem.as_bytes()[slot * SLOT + 4096..(slot + 1) * SLOT - 4];
        assert!(last_sector.iter().any(|b| *b != 0xFF), "slot {slot}");
    }
    mem
}

#[test]
fn interrupted_erase_of_a_committed_slot_is_not_corruption() {
    // The inactive slot holds an older committed configuration. Its commit
    // word is cleared before the erase, so a partially erased slot is never
    // committed, even when the document spans the sector holding the word.
    for base in [base_config(), large_base_config()] {
        let active = open(clone(&base)).active_slot();
        for budget in (0..=SLOT).step_by(64) {
            let mut mem = clone(&base);
            mem.fail_erase_after(budget);
            let mut store = open(mem);
            let result = store.set(&wifi(2));
            let mut mem = store.into_storage();
            mem.clear_faults();
            let store = open(mem);
            assert_eq!(
                store.open_state(),
                ConfigOpenState::Valid,
                "budget {budget}"
            );
            if result.is_err() {
                assert_eq!(store.active_slot(), active, "budget {budget}");
            }
        }
    }
}

#[test]
fn inactive_commit_word_is_cleared_before_erasing() {
    let base = base_config();
    let inactive = 1 - open(clone(&base)).active_slot().unwrap();
    let word = (inactive + 1) * SLOT - 4;
    assert_eq!(&base.as_bytes()[word..word + 4], &COMMIT_WORD.to_le_bytes());

    let mut mem = clone(&base);
    mem.fail_erase_after(0);
    let mut store = open(mem);
    assert!(store.set(&wifi(2)).is_err());
    let mem = store.into_storage();
    // Nothing was erased, but the old document is no longer committed.
    assert_eq!(&mem.as_bytes()[word..word + 4], &[0; 4]);
    assert_eq!(
        &mem.as_bytes()[inactive * SLOT..inactive * SLOT + 32],
        &base.as_bytes()[inactive * SLOT..inactive * SLOT + 32]
    );
    let mut mem = mem;
    mem.clear_faults();
    let mut store = open(mem);
    assert_eq!(store.open_state(), ConfigOpenState::Valid);
    assert_eq!(store.get::<WifiConfig>().unwrap(), Some(wifi(1)));
}

#[test]
fn first_write_to_empty_storage_is_atomic() {
    let mut probe = open(Mem::new());
    probe.set(&wifi(1)).unwrap();
    let total = probe.storage().bytes_written();
    for budget in 0..=total {
        let mut mem = Mem::new();
        mem.fail_write_after(budget);
        let mut store = open(mem);
        let result = store.set(&wifi(1));
        let mut mem = store.into_storage();
        mem.clear_faults();
        // An interrupted first write is never corruption: empty or complete.
        let store = ConfigStore::open(mem).unwrap();
        let mut store = store;
        let w = store.get::<WifiConfig>().unwrap();
        match result {
            Ok(()) => assert_eq!(w, Some(wifi(1))),
            Err(_) => {
                assert!(w.is_none() || w == Some(wifi(1)), "budget {budget}");
                if w.is_none() {
                    assert_eq!(
                        store.open_state(),
                        ConfigOpenState::Empty,
                        "budget {budget}"
                    );
                }
            }
        }
    }
}

#[test]
fn repeated_updates_with_faults_keep_a_valid_configuration() {
    let mut mem = Mem::new();
    let mut last_ok = 0u32;
    for round in 1..=40u32 {
        mem.fail_write_after((round as usize * 37) % 200);
        let mut store = open(mem);
        if store.set(&connectivity(round)).is_ok() {
            last_ok = round;
        }
        mem = store.into_storage();
        mem.clear_faults();
        let (m, _, c) = state(mem);
        mem = m;
        let value = c.map(|c| c.interval_s).unwrap_or(0);
        assert!(value == last_ok || value == round, "round {round}: {value}");
        last_ok = value;
    }
}

#[test]
fn storage_errors_are_reported() {
    let mut mem = Mem::new();
    mem.fail_write_after(0);
    let mut store = open(mem);
    assert!(matches!(
        store.set(&connectivity(1)),
        Err(ConfigError::Storage(MemoryError::Injected))
    ));
    assert_eq!(store.generation(), None);
}

mod fuzz {
    use super::*;
    use proptest::prelude::*;

    fn exercise(mem: Mem) {
        if let Ok(mut store) = ConfigStore::open(mem) {
            let _ = store.get::<WifiConfig>();
            let _ = store.get::<ConnectivityConfig>();
            let _ = store.for_each_section(|_| {});
            let _ = store.read_raw(Identity::ID, &mut [0u8; 64]);
            let _ = store.set(&connectivity(1));
            let _ = store.get::<ConnectivityConfig>();
            let _ = store.clear();
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn arbitrary_storage_contents_never_panic(
            data in proptest::collection::vec(any::<u8>(), 0..512),
            offset in 0usize..2,
            committed in any::<bool>(),
        ) {
            let mut image = vec![0xFF; SLOT];
            image[..data.len()].copy_from_slice(&data);
            if committed {
                image[SLOT - 4..].copy_from_slice(&COMMIT_WORD.to_le_bytes());
            }
            let mut mem = Mem::new();
            install(&mut mem, offset, &image);
            exercise(mem);
        }

        #[test]
        fn arbitrary_payload_with_valid_crc_never_panics(
            payload in proptest::collection::vec(any::<u8>(), 0..256),
            offset in 0usize..2,
        ) {
            let mut mem = Mem::new();
            install(&mut mem, offset, &slot_image(&envelope(&payload), true));
            exercise(mem);
        }

        #[test]
        fn arbitrary_records_never_panic(
            raw in proptest::collection::vec(
                proptest::collection::vec(any::<u8>(), 0..48), 0..6),
            generation in any::<u32>(),
        ) {
            let mut mem = Mem::new();
            install(&mut mem, 0, &valid_image(generation, &raw));
            exercise(mem);
        }

        #[test]
        fn random_headers_with_valid_framing_never_panic(
            id in any::<u64>(),
            version in any::<u16>(),
            payload in proptest::collection::vec(any::<u8>(), 0..64),
            generation in any::<u32>(),
        ) {
            let mut mem = Mem::new();
            install(&mut mem, 1, &valid_image(generation, &[record(id, version, &payload)]));
            let mut store = open(mem);
            prop_assert_eq!(store.open_state(), ConfigOpenState::Valid);
            let _ = store.get::<ConnectivityConfig>();
            let mut out = [0u8; 64];
            let info = store.read_raw(SectionId(id), &mut out).unwrap().unwrap();
            prop_assert_eq!(&out[..info.length], &payload[..]);
            prop_assert_eq!(info.version, version);
        }

        #[test]
        fn single_byte_corruption_never_yields_foreign_data(
            index in 0usize..2 * SLOT,
            flip in 1u8..=255,
        ) {
            let mut store = open(Mem::new());
            store.set(&connectivity(1)).unwrap();
            store.set(&connectivity(2)).unwrap();
            let mut mem = store.into_storage();
            mem.as_bytes_mut()[index] ^= flip;
            match ConfigStore::open(mem) {
                Ok(mut store) => {
                    let value = store.get::<ConnectivityConfig>().unwrap();
                    prop_assert!(value == Some(connectivity(1)) || value == Some(connectivity(2)));
                }
                Err(error) => prop_assert!(matches!(error, ConfigError::CorruptStorage(_))),
            }
        }
    }
}

#[test]
fn remove_touches_only_its_section_and_keeps_unknown_ones() {
    const PROTECTED: SectionId = SectionId(0xDEAD_BEEF_0000_0002);
    const PAYLOAD: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    store.set(&identity(7)).unwrap();
    store.set_raw(PROTECTED, 3, &PAYLOAD).unwrap();

    store.remove::<WifiConfig>().unwrap();

    assert_eq!(store.get::<WifiConfig>().unwrap(), None);
    assert_eq!(store.get::<Identity>().unwrap(), Some(identity(7)));
    let mut raw = [0u8; 8];
    let info = store.read_raw(PROTECTED, &mut raw).unwrap().unwrap();
    assert_eq!((info.version, &raw[..info.length]), (3, &PAYLOAD[..]));
}

#[test]
fn format_is_a_separate_low_level_operation() {
    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    store.set(&identity(7)).unwrap();

    // Formatting is a storage operation, not a section mutation: it discards
    // every section, which `remove` never does.
    let store = match ConfigStore::format(store.into_storage()) {
        Ok(store) => store,
        Err(_) => panic!("format failed"),
    };
    assert_eq!(store.section_count(), 0);
}
