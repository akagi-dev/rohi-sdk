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

//! Host tests of the transactional configuration store.

use postcard::experimental::max_size::MaxSize;
use rohi_config::{
    COMMIT_MARKER, ConfigError, ConfigSection, ConfigStore, Crc32, MAX_SECTIONS, MemoryError,
    MemoryStorage, SLOT_HEADER_SIZE, SLOT_MAGIC, SectionError, SectionHeader, SectionId,
    SectionInfo,
};
use serde::{Deserialize, Serialize};

type Mem = MemoryStorage<16384, 4096>;
const SLOT: usize = 8192;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(name = "wifi", version = 1)]
struct WifiConfig {
    ssid_len: u8,
    ssid: [u8; 32],
    password_len: u8,
    password: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(name = "telemetry", version = 1)]
struct TelemetryConfig {
    interval_s: u32,
    enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(version = 1)]
struct DefaultNamed {
    value: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(id = 0xAABBCCDDEEFF0011, version = 1)]
struct ExplicitId {
    value: u8,
}

// Same section as `Small`, with a field more: used to provoke trailing bytes.
#[derive(Debug, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(name = "shape", version = 1)]
struct Small {
    a: u8,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(name = "shape", version = 1)]
struct Wide {
    a: u8,
    b: u8,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(id = 0x1000, version = 1)]
struct MigratedV1 {
    a: u8,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(id = 0x1000, version = 2, decode_versioned = "migrate")]
struct MigratedV2 {
    a: u8,
    b: u8,
}

fn migrate(version: u16, payload: &[u8]) -> Result<MigratedV2, SectionError> {
    match version {
        1 => {
            let old: MigratedV1 = rohi_config::decode_all(payload)?;
            Ok(MigratedV2 { a: old.a, b: 99 })
        }
        2 => rohi_config::decode_all(payload),
        version => Err(SectionError::UnsupportedVersion {
            id: MigratedV2::ID,
            version,
        }),
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(id = 0x2000, version = 1)]
struct Huge {
    data: [[u8; 32]; 32],
}

#[derive(Debug, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(id = 0x2001, version = 1)]
struct TooHuge {
    data: [[u8; 32]; 32],
    extra: u8,
}

macro_rules! tiny_sections {
    ($($name:ident = $id:literal),*) => {
        $(
            #[derive(Debug, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
            #[config(id = $id, version = 1)]
            struct $name(u8);
        )*
    };
}

tiny_sections!(
    T0 = 0x3000,
    T1 = 0x3001,
    T2 = 0x3002,
    T3 = 0x3003,
    T4 = 0x3004,
    T5 = 0x3005,
    T6 = 0x3006,
    T7 = 0x3007,
    T8 = 0x3008,
    T9 = 0x3009,
    T10 = 0x300A,
    T11 = 0x300B,
    T12 = 0x300C,
    T13 = 0x300D,
    T14 = 0x300E,
    T15 = 0x300F,
    T16 = 0x3010
);

fn wifi(tag: u8) -> WifiConfig {
    WifiConfig {
        ssid_len: 4,
        ssid: [tag; 32],
        password_len: 8,
        password: [tag.wrapping_add(1); 32],
    }
}

fn telemetry(interval_s: u32) -> TelemetryConfig {
    TelemetryConfig {
        interval_s,
        enabled: true,
    }
}

fn open(mem: Mem) -> ConfigStore<Mem> {
    ConfigStore::open(mem).unwrap()
}

/// Builds a container slot image with a valid CRC and commit marker.
fn slot_image(generation: u32, payload: &[u8], payload_length: u32) -> Vec<u8> {
    let mut header = [0u8; SLOT_HEADER_SIZE];
    header[0..4].copy_from_slice(&SLOT_MAGIC.to_le_bytes());
    header[4..6].copy_from_slice(&1u16.to_le_bytes());
    header[8..12].copy_from_slice(&generation.to_le_bytes());
    header[12..16].copy_from_slice(&payload_length.to_le_bytes());
    let mut crc = Crc32::new();
    crc.update(payload);
    crc.update(&header[4..16]);
    header[16..20].copy_from_slice(&crc.finish().to_le_bytes());
    header[20..24].copy_from_slice(&COMMIT_MARKER.to_le_bytes());
    let mut image = header.to_vec();
    image.extend_from_slice(payload);
    image
}

fn section_bytes(id: u64, version: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = SectionHeader {
        id: SectionId(id),
        version,
        length: payload.len() as u32,
    }
    .to_bytes()
    .to_vec();
    bytes.extend_from_slice(payload);
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    bytes
}

fn install(mem: &mut Mem, slot: usize, image: &[u8]) {
    mem.as_bytes_mut()[slot * SLOT..slot * SLOT + image.len()].copy_from_slice(image);
}

#[test]
fn empty_storage_opens_empty() {
    let mut store = open(Mem::new());
    assert_eq!(store.generation(), None);
    assert_eq!(store.get::<WifiConfig>().unwrap(), None);
    assert_eq!(store.section_count(), 0);
}

#[test]
fn typed_round_trip_survives_reopen() {
    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    assert_eq!(store.get::<WifiConfig>().unwrap(), Some(wifi(1)));
    let mut store = open(store.into_storage());
    assert_eq!(store.get::<WifiConfig>().unwrap(), Some(wifi(1)));
    assert_eq!(store.generation(), Some(1));
}

#[test]
fn multiple_sections_are_independent() {
    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    store.set(&telemetry(60)).unwrap();
    store.set(&DefaultNamed { value: 7 }).unwrap();
    store.set(&ExplicitId { value: 9 }).unwrap();
    let mut store = open(store.into_storage());
    assert_eq!(store.get::<WifiConfig>().unwrap(), Some(wifi(1)));
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(60)));
    assert_eq!(
        store.get::<DefaultNamed>().unwrap(),
        Some(DefaultNamed { value: 7 })
    );
    assert_eq!(
        store.get::<ExplicitId>().unwrap(),
        Some(ExplicitId { value: 9 })
    );
    assert_eq!(store.section_count(), 4);
}

#[test]
fn derive_metadata_is_stable() {
    assert_eq!(WifiConfig::ID, SectionId(0xa63b_01f6_5577_96a8));
    assert_eq!(WifiConfig::NAME, "wifi");
    assert_eq!(TelemetryConfig::ID, SectionId(0xa042_ab53_f67e_07c0));
    assert_eq!(DefaultNamed::NAME, "DefaultNamed");
    assert_eq!(DefaultNamed::ID, SectionId::from_name("DefaultNamed"));
    assert_eq!(ExplicitId::ID, SectionId(0xAABB_CCDD_EEFF_0011));
    assert_eq!(MigratedV2::VERSION, 2);
}

#[test]
fn replacement_preserves_unrelated_sections_and_order() {
    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    store.set(&telemetry(60)).unwrap();
    store.set(&wifi(2)).unwrap();
    assert_eq!(store.get::<WifiConfig>().unwrap(), Some(wifi(2)));
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(60)));
    let mut ids = Vec::new();
    store.for_each_section(|info| ids.push(info.id)).unwrap();
    assert_eq!(ids, [WifiConfig::ID, TelemetryConfig::ID]);
    assert_eq!(store.generation(), Some(3));
}

#[test]
fn remove_and_clear() {
    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    store.set(&telemetry(60)).unwrap();
    store.remove::<WifiConfig>().unwrap();
    assert_eq!(store.get::<WifiConfig>().unwrap(), None);
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(60)));

    // Removing an absent section does not write.
    let generation = store.generation();
    store.remove::<WifiConfig>().unwrap();
    assert_eq!(store.generation(), generation);

    store.clear().unwrap();
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), None);
    let mut store = open(store.into_storage());
    assert_eq!(store.section_count(), 0);
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), None);
    // Clearing an empty store does not write either.
    let generation = store.generation();
    store.clear().unwrap();
    assert_eq!(store.generation(), generation);
}

#[test]
fn encoded_size_respects_max_size() {
    let mut buffer = [0u8; 256];
    let len = rohi_config::encode_to_slice(&wifi(3), &mut buffer).unwrap();
    assert!(len <= WifiConfig::POSTCARD_MAX_SIZE);
    let len = rohi_config::encode_to_slice(&telemetry(u32::MAX), &mut buffer).unwrap();
    assert!(len <= TelemetryConfig::POSTCARD_MAX_SIZE);
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut store = open(Mem::new());
    store.set(&Wide { a: 1, b: 2 }).unwrap();
    match store.get::<Small>() {
        Err(ConfigError::Section(SectionError::Decode(_))) => {}
        other => panic!("expected decode error, got {other:?}"),
    }
    assert_eq!(store.get::<Wide>().unwrap(), Some(Wide { a: 1, b: 2 }));
}

#[test]
fn truncated_payload_is_a_typed_error() {
    let mut store = open(Mem::new());
    store.set(&Small { a: 1 }).unwrap();
    match store.get::<Wide>() {
        Err(ConfigError::Section(SectionError::Decode(_))) => {}
        other => panic!("expected decode error, got {other:?}"),
    }
}

#[test]
fn unsupported_version_without_migration_hook() {
    let mut mem = Mem::new();
    let payload = section_bytes(TelemetryConfig::ID.0, 9, &[60, 1]);
    install(&mut mem, 0, &slot_image(1, &payload, payload.len() as u32));
    let mut store = open(mem);
    assert!(matches!(
        store.get::<TelemetryConfig>(),
        Err(ConfigError::Section(SectionError::UnsupportedVersion {
            version: 9,
            ..
        }))
    ));
}

#[test]
fn migration_hook_upgrades_old_versions_only_for_its_section() {
    let mut store = open(Mem::new());
    store.set(&MigratedV1 { a: 5 }).unwrap();
    store.set(&telemetry(30)).unwrap();
    assert_eq!(
        store.get::<MigratedV2>().unwrap(),
        Some(MigratedV2 { a: 5, b: 99 })
    );
    store.set(&MigratedV2 { a: 6, b: 7 }).unwrap();
    assert_eq!(
        store.get::<MigratedV2>().unwrap(),
        Some(MigratedV2 { a: 6, b: 7 })
    );
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(30)));
}

#[test]
fn unknown_sections_survive_updates_and_removals() {
    let unknown_payload: Vec<u8> = (0..37).collect();
    let mut mem = Mem::new();
    let mut payload = section_bytes(0xDEAD_BEEF_0000_0001, 7, &unknown_payload);
    payload.extend(section_bytes(WifiConfig::ID.0, 1, &{
        let mut buf = [0u8; 128];
        let len = rohi_config::encode_to_slice(&wifi(1), &mut buf).unwrap();
        buf[..len].to_vec()
    }));
    install(&mut mem, 0, &slot_image(5, &payload, payload.len() as u32));

    let mut store = open(mem);
    assert_eq!(store.get::<WifiConfig>().unwrap(), Some(wifi(1)));

    for step in 0..4 {
        match step {
            0 => store.set(&wifi(2)).unwrap(),
            1 => store.set(&telemetry(10)).unwrap(),
            2 => store.remove::<WifiConfig>().unwrap(),
            _ => store.set(&wifi(3)).unwrap(),
        }
        let mut out = [0u8; 64];
        let info = store
            .read_raw(SectionId(0xDEAD_BEEF_0000_0001), &mut out)
            .unwrap()
            .expect("unknown section lost");
        assert_eq!(
            info,
            SectionInfo {
                id: SectionId(0xDEAD_BEEF_0000_0001),
                version: 7,
                length: 37
            }
        );
        assert_eq!(&out[..37], unknown_payload.as_slice());
    }

    // Byte-for-byte identical on flash, including header and padding.
    let storage = store.into_storage();
    let slot = if &storage.as_bytes()[0..4] == b"ROHC"
        && storage.as_bytes()[8] > storage.as_bytes()[SLOT + 8]
    {
        0
    } else {
        1
    };
    let base = slot * SLOT + SLOT_HEADER_SIZE;
    let expected = section_bytes(0xDEAD_BEEF_0000_0001, 7, &unknown_payload);
    assert_eq!(
        &storage.as_bytes()[base..base + expected.len()],
        expected.as_slice()
    );
}

#[test]
fn unknown_section_removed_only_explicitly() {
    let mut mem = Mem::new();
    let payload = section_bytes(0x42, 1, &[1, 2, 3]);
    install(&mut mem, 0, &slot_image(1, &payload, payload.len() as u32));
    let mut store = open(mem);
    assert_eq!(store.section_count(), 1);
    store.remove_raw(SectionId(0x42)).unwrap();
    assert_eq!(store.section_count(), 0);
}

#[test]
fn duplicate_section_ids_invalidate_the_container() {
    let mut mem = Mem::new();
    let mut payload = section_bytes(0x42, 1, &[1]);
    payload.extend(section_bytes(0x42, 1, &[2]));
    install(&mut mem, 0, &slot_image(1, &payload, payload.len() as u32));
    let store = open(mem);
    assert_eq!(store.generation(), None);
}

#[test]
fn duplicate_ids_fall_back_to_previous_generation() {
    let mut store = open(Mem::new());
    store.set(&telemetry(60)).unwrap();
    let mut mem = store.into_storage();
    let mut payload = section_bytes(0x42, 1, &[1]);
    payload.extend(section_bytes(0x42, 1, &[2]));
    install(&mut mem, 1, &slot_image(2, &payload, payload.len() as u32));
    let mut store = open(mem);
    assert_eq!(store.generation(), Some(1));
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(60)));
}

#[test]
fn too_many_sections_in_container_is_invalid() {
    let mut payload = Vec::new();
    for id in 0..=MAX_SECTIONS as u64 {
        payload.extend(section_bytes(id + 1, 1, &[1]));
    }
    let mut mem = Mem::new();
    install(&mut mem, 0, &slot_image(1, &payload, payload.len() as u32));
    assert_eq!(open(mem).generation(), None);
}

#[test]
fn crc_corruption_falls_back_to_previous_slot() {
    let mut store = open(Mem::new());
    store.set(&telemetry(1)).unwrap();
    store.set(&telemetry(2)).unwrap();
    assert_eq!(store.generation(), Some(2));
    let mut mem = store.into_storage();
    // Generation 2 lives in slot B.
    mem.as_bytes_mut()[SLOT + SLOT_HEADER_SIZE + 16] ^= 0x01;
    let mut store = open(mem);
    assert_eq!(store.generation(), Some(1));
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(1)));
    // The next commit goes to the corrupt slot and wins.
    store.set(&telemetry(3)).unwrap();
    let mut store = open(store.into_storage());
    assert_eq!(store.generation(), Some(2));
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(3)));
}

#[test]
fn uncommitted_slot_is_ignored() {
    let mut store = open(Mem::new());
    store.set(&telemetry(1)).unwrap();
    store.set(&telemetry(2)).unwrap();
    let mut mem = store.into_storage();
    // Wipe the commit marker of the newest slot: like an interrupted commit.
    let marker = SLOT + 20;
    mem.as_bytes_mut()[marker..marker + 4].fill(0xFF);
    let mut store = open(mem);
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(1)));
}

#[test]
fn malformed_containers_are_rejected_without_panics() {
    let mut cases: Vec<(Vec<u8>, u32)> = Vec::new();
    // Section length beyond the container.
    let mut p = section_bytes(1, 1, &[1, 2, 3, 4]);
    p[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push((p.clone(), p.len() as u32));
    // Section length beyond the payload but within MAX_SECTION_PAYLOAD.
    let mut p = section_bytes(1, 1, &[1, 2, 3, 4]);
    p[12..16].copy_from_slice(&512u32.to_le_bytes());
    cases.push((p.clone(), p.len() as u32));
    // Truncated section header.
    cases.push((vec![1, 2, 3, 4, 5, 6, 7, 8], 8));
    // Payload length unaligned / beyond slot / absurd.
    let p = section_bytes(1, 1, &[1]);
    cases.push((p.clone(), p.len() as u32 - 1));
    cases.push((p.clone(), SLOT as u32));
    cases.push((p.clone(), u32::MAX));
    cases.push((p.clone(), u32::MAX - 3));
    for (payload, length) in cases {
        let mut mem = Mem::new();
        install(&mut mem, 0, &slot_image(1, &payload, length));
        let mut store = open(mem);
        assert_eq!(
            store.generation(),
            None,
            "payload {payload:?} length {length}"
        );
        assert_eq!(store.get::<TelemetryConfig>().unwrap(), None);
    }
}

#[test]
fn generation_wraps_around() {
    let mut mem = Mem::new();
    let payload = section_bytes(TelemetryConfig::ID.0, 1, &[60, 1]);
    install(
        &mut mem,
        0,
        &slot_image(u32::MAX, &payload, payload.len() as u32),
    );
    let mut store = open(mem);
    assert_eq!(store.generation(), Some(u32::MAX));
    store.set(&telemetry(61)).unwrap();
    assert_eq!(store.generation(), Some(0));
    let mut store = open(store.into_storage());
    assert_eq!(store.generation(), Some(0));
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(61)));
    store.set(&telemetry(62)).unwrap();
    assert_eq!(store.generation(), Some(1));
    let mut store = open(store.into_storage());
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), Some(telemetry(62)));
}

#[test]
fn size_limits_are_enforced() {
    let mut store = open(Mem::new());
    assert!(matches!(
        store.set(&TooHuge {
            data: [[0; 32]; 32],
            extra: 0
        }),
        Err(ConfigError::SectionTooLarge { max: 1024, .. })
    ));
    store
        .set(&Huge {
            data: [[7; 32]; 32],
        })
        .unwrap();
    assert_eq!(store.get::<Huge>().unwrap().unwrap().data[31][31], 7);
    assert_eq!(store.generation(), Some(1));
}

#[test]
fn container_full_leaves_previous_configuration_untouched() {
    let mut store = open(Mem::new());
    store.set(&telemetry(5)).unwrap();
    // Seven maximum-sized unknown sections leave no room for another one.
    let mut mem = store.into_storage();
    let mut payload = Vec::new();
    for id in 0..7u64 {
        payload.extend(section_bytes(0x5000 + id, 1, &[0u8; 1024]));
    }
    install(&mut mem, 1, &slot_image(2, &payload, payload.len() as u32));
    let mut store = open(mem);
    assert_eq!(store.generation(), Some(2));
    assert!(matches!(
        store.set(&Huge {
            data: [[1; 32]; 32]
        }),
        Err(ConfigError::ContainerFull)
    ));
    assert_eq!(store.generation(), Some(2));
    let store = open(store.into_storage());
    assert_eq!(store.generation(), Some(2));
    assert_eq!(store.section_count(), 7);
}

#[test]
fn section_count_limit_is_enforced() {
    let mut store = open(Mem::new());
    store.set(&T0(0)).unwrap();
    store.set(&T1(1)).unwrap();
    store.set(&T2(2)).unwrap();
    store.set(&T3(3)).unwrap();
    store.set(&T4(4)).unwrap();
    store.set(&T5(5)).unwrap();
    store.set(&T6(6)).unwrap();
    store.set(&T7(7)).unwrap();
    store.set(&T8(8)).unwrap();
    store.set(&T9(9)).unwrap();
    store.set(&T10(10)).unwrap();
    store.set(&T11(11)).unwrap();
    store.set(&T12(12)).unwrap();
    store.set(&T13(13)).unwrap();
    store.set(&T14(14)).unwrap();
    store.set(&T15(15)).unwrap();
    assert_eq!(store.section_count(), MAX_SECTIONS);
    assert!(matches!(
        store.set(&T16(16)),
        Err(ConfigError::TooManySections)
    ));
    // Replacing an existing section is still allowed.
    store.set(&T3(33)).unwrap();
    assert_eq!(store.get::<T3>().unwrap(), Some(T3(33)));
    assert_eq!(store.get::<T16>().unwrap(), None);
}

#[test]
fn read_raw_reports_small_buffers() {
    let mut store = open(Mem::new());
    store.set(&wifi(1)).unwrap();
    let mut out = [0u8; 2];
    assert!(matches!(
        store.read_raw(WifiConfig::ID, &mut out),
        Err(ConfigError::BufferTooSmall { .. })
    ));
    assert_eq!(store.read_raw(TelemetryConfig::ID, &mut out).unwrap(), None);
}

#[test]
fn storage_geometry_is_validated() {
    assert!(matches!(
        ConfigStore::open(MemoryStorage::<4096, 4096>::new()),
        Err(ConfigError::InvalidStorage(_))
    ));
}

fn state(mem: Mem) -> (Mem, Option<WifiConfig>, Option<TelemetryConfig>) {
    let mut store = open(mem);
    let w = store.get::<WifiConfig>().unwrap();
    let t = store.get::<TelemetryConfig>().unwrap();
    (store.into_storage(), w, t)
}

#[test]
fn interrupted_write_never_loses_the_previous_configuration() {
    let mut base = open(Mem::new());
    base.set(&wifi(1)).unwrap();
    base.set(&telemetry(60)).unwrap();
    let base = base.into_storage();

    // Measure the bytes programmed by one update.
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
        let (_, w, t) = state(mem);
        assert_eq!(t, Some(telemetry(60)), "budget {budget}");
        if result.is_ok() {
            assert_eq!(w, Some(wifi(2)), "budget {budget}");
        } else {
            assert!(w == Some(wifi(1)) || w == Some(wifi(2)), "budget {budget}");
        }
    }
}

#[test]
fn interrupted_write_before_the_marker_keeps_the_old_value() {
    let mut base = open(Mem::new());
    base.set(&wifi(1)).unwrap();
    let base = base.into_storage();
    let mut probe = open(clone(&base));
    let before = probe.storage().bytes_written();
    probe.set(&wifi(2)).unwrap();
    let total = probe.storage().bytes_written() - before;
    // Everything but the 4-byte commit marker lets the update fail.
    for budget in 0..total - 4 {
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
    let mut base = open(Mem::new());
    base.set(&wifi(1)).unwrap();
    let base = base.into_storage();
    for budget in (0..=SLOT).step_by(512) {
        let mut mem = clone(&base);
        mem.fail_erase_after(budget);
        let mut store = open(mem);
        let result = store.set(&wifi(2));
        let mut mem = store.into_storage();
        mem.clear_faults();
        let w = state(mem).1;
        if result.is_ok() {
            assert_eq!(w, Some(wifi(2)), "budget {budget}");
        } else {
            assert_eq!(w, Some(wifi(1)), "budget {budget}");
        }
    }
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
        let w = state(mem).1;
        if result.is_ok() {
            assert_eq!(w, Some(wifi(1)));
        } else {
            assert!(w.is_none() || w == Some(wifi(1)), "budget {budget}");
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
        if store.set(&telemetry(round)).is_ok() {
            last_ok = round;
        }
        mem = store.into_storage();
        mem.clear_faults();
        let (m, _, t) = state(mem);
        mem = m;
        let value = t.map(|t| t.interval_s).unwrap_or(0);
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
        store.set(&telemetry(1)),
        Err(ConfigError::Storage(MemoryError::Injected))
    ));
    assert_eq!(store.generation(), None);
}

fn clone(mem: &Mem) -> Mem {
    let mut copy = Mem::new();
    copy.as_bytes_mut().copy_from_slice(mem.as_bytes());
    copy
}

mod fuzz {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn arbitrary_storage_contents_never_panic(
            data in proptest::collection::vec(any::<u8>(), 0..512),
            offset in 0usize..2,
        ) {
            let mut mem = Mem::new();
            mem.as_bytes_mut()[offset * SLOT..offset * SLOT + data.len()].copy_from_slice(&data);
            let mut store = open(mem);
            let _ = store.get::<WifiConfig>();
            let _ = store.for_each_section(|_| {});
            let _ = store.set(&telemetry(1));
            let _ = store.clear();
        }

        #[test]
        fn plausible_headers_with_arbitrary_payload_never_panic(
            payload in proptest::collection::vec(any::<u8>(), 0..256),
            length in any::<u32>(),
            generation in any::<u32>(),
        ) {
            let mut mem = Mem::new();
            install(&mut mem, 0, &slot_image(generation, &payload, length));
            let mut store = open(mem);
            let _ = store.get::<TelemetryConfig>();
            let _ = store.for_each_section(|_| {});
            let _ = store.set(&telemetry(2));
        }

        #[test]
        fn single_byte_corruption_never_yields_foreign_data(
            index in 0usize..2 * SLOT,
            flip in 1u8..=255,
        ) {
            let mut store = open(Mem::new());
            store.set(&telemetry(1)).unwrap();
            store.set(&telemetry(2)).unwrap();
            let mut mem = store.into_storage();
            mem.as_bytes_mut()[index] ^= flip;
            let mut store = open(mem);
            let value = store.get::<TelemetryConfig>().unwrap();
            prop_assert!(value == Some(telemetry(1)) || value == Some(telemetry(2)));
        }
    }
}
