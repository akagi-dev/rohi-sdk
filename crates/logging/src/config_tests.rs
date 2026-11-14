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

use rohi_config::{ConfigOverrides, ConfigSection, Override, SectionId, VariantValue};
use rohi_service::{Config, FieldError, MAX_CONFIG_FIELDS, Unit, Value};

use crate::*;

const LEVEL: u32 = 1;
const STORAGE: u32 = 2;
const COMPRESSION: u32 = 3;
const RETENTION: u32 = 4;
const SEGMENT: u32 = 5;
const FLUSH: u32 = 6;

#[test]
fn the_defaults_are_valid_and_conservative() {
    let config = LoggingConfig::default();
    assert_eq!(config.validate(), Ok(()));
    assert_eq!(Config::validate(&config), Ok(()));
    let read = |id| config.read_field(id).unwrap();
    assert_eq!(read(LEVEL), Some(Value::Enum(2)));
    assert_eq!(read(STORAGE), Some(Value::Enum(0)));
    assert_eq!(read(COMPRESSION), Some(Value::Enum(1)));
    assert_eq!(read(RETENTION), Some(Value::U64(1024 * 1024)));
    assert_eq!(read(SEGMENT), Some(Value::U64(64 * 1024)));
    assert_eq!(read(FLUSH), Some(Value::U64(10_000)));
}

#[test]
fn the_profile_selects_the_persistent_level() {
    let level = |development| {
        let overrides = LoggingConfig::firmware_overrides(development);
        rohi_config::materialize::<LoggingConfig>(&overrides, &Default::default()).level
    };
    assert_eq!(level(false), LogLevel::Warn);
    assert_eq!(level(true), LogLevel::Info);

    // A user override beats the firmware policy; removing it restores it.
    let user = LoggingOverrides {
        level: Override::Value(LogLevel::Debug),
        ..Default::default()
    };
    let firmware = LoggingConfig::firmware_overrides(false);
    assert_eq!(
        rohi_config::materialize::<LoggingConfig>(&firmware, &user).level,
        LogLevel::Debug
    );
}

#[test]
fn every_field_is_set_by_its_id_and_the_kind_is_checked() {
    let mut config = LoggingConfig::default();
    config.set_field(LEVEL, Value::Enum(4)).unwrap();
    config.set_field(STORAGE, Value::Enum(1)).unwrap();
    config.set_field(COMPRESSION, Value::Enum(0)).unwrap();
    config.set_field(RETENTION, Value::U64(256 * 1024)).unwrap();
    config.set_field(SEGMENT, Value::U64(16 * 1024)).unwrap();
    config.set_field(FLUSH, Value::U64(2_000)).unwrap();
    assert_eq!(
        config,
        LoggingConfig {
            level: LogLevel::Debug,
            storage: LogStorageKind::Sd,
            compression: Compression::None,
            retention_bytes: 256 * 1024,
            segment_bytes: 16 * 1024,
            flush_interval_ms: 2_000,
        }
    );
    assert_eq!(Config::validate(&config), Ok(()));

    assert_eq!(
        config.set_field(LEVEL, Value::Enum(5)),
        Err(FieldError::OutOfRange)
    );
    assert_eq!(
        config.set_field(LEVEL, Value::text("debug").unwrap()),
        Err(FieldError::WrongKind)
    );
    for id in [STORAGE, COMPRESSION] {
        assert_eq!(
            config.set_field(id, Value::text("lz4").unwrap()),
            Err(FieldError::WrongKind)
        );
    }
    assert_eq!(
        config.set_field(STORAGE, Value::Enum(2)),
        Err(FieldError::OutOfRange)
    );
    assert_eq!(
        config.set_field(COMPRESSION, Value::Enum(2)),
        Err(FieldError::OutOfRange)
    );
    assert_eq!(
        config.set_field(RETENTION, Value::Bool(true)),
        Err(FieldError::WrongKind)
    );
}

#[test]
fn invalid_combinations_are_rejected_as_a_whole() {
    let defaults = LoggingConfig::default();
    for config in [
        // One segment cannot be rotated.
        LoggingConfig {
            retention_bytes: 8 * 1024,
            ..defaults
        },
        // A segment larger than half the budget.
        LoggingConfig {
            segment_bytes: 64 * 1024,
            retention_bytes: 96 * 1024,
            ..defaults
        },
        // Not a multiple of the erase sector.
        LoggingConfig {
            segment_bytes: 6 * 1024,
            ..defaults
        },
        // Out of range.
        LoggingConfig {
            segment_bytes: 2 * 1024,
            ..defaults
        },
        LoggingConfig {
            segment_bytes: 128 * 1024,
            retention_bytes: 4 * 1024 * 1024,
            ..defaults
        },
        LoggingConfig {
            retention_bytes: 128 * 1024 * 1024,
            ..defaults
        },
        LoggingConfig {
            flush_interval_ms: 10,
            ..defaults
        },
        LoggingConfig {
            flush_interval_ms: 3_600_000,
            ..defaults
        },
    ] {
        assert_eq!(config.validate(), Err(FieldError::Invalid), "{config:?}");
        assert_eq!(
            Config::validate(&config),
            Err(FieldError::Invalid),
            "{config:?}"
        );
    }

    // The boundaries are valid.
    for config in [
        LoggingConfig {
            retention_bytes: 8 * 1024,
            segment_bytes: 4 * 1024,
            ..defaults
        },
        LoggingConfig {
            retention_bytes: 128 * 1024,
            segment_bytes: 64 * 1024,
            ..defaults
        },
        LoggingConfig {
            retention_bytes: 64 * 1024 * 1024,
            ..defaults
        },
        LoggingConfig {
            flush_interval_ms: 600_000,
            ..defaults
        },
    ] {
        assert_eq!(config.validate(), Ok(()), "{config:?}");
    }
}

#[test]
fn a_rejected_patch_leaves_the_stored_overrides_alone() {
    let stored = LoggingOverrides {
        retention_bytes: Override::Value(256 * 1024),
        ..Default::default()
    };
    // The patch is applied to a copy of the overrides; the copy is invalid.
    let mut candidate = stored.clone();
    let mut effective = rohi_config::materialize::<LoggingConfig>(&Default::default(), &stored);
    effective
        .set_field(RETENTION, Value::U64(64 * 1024))
        .unwrap();
    assert!(candidate.capture(&effective, RETENTION));
    let applied = rohi_config::materialize::<LoggingConfig>(&Default::default(), &candidate);
    assert_eq!(Config::validate(&applied), Err(FieldError::Invalid));

    let still = rohi_config::materialize::<LoggingConfig>(&Default::default(), &stored);
    assert_eq!(Config::validate(&still), Ok(()));
    assert_eq!(still.retention_bytes, 256 * 1024);
    assert_eq!(still.segment_bytes, DEFAULT_SEGMENT_BYTES);
}

#[test]
fn an_override_of_the_former_defaults_stays_authoritative() {
    // A budget that suited the 8 KiB default segment is too small for the
    // 64 KiB one: the section is invalid until the segment size is set too.
    let old = LoggingOverrides {
        retention_bytes: Override::Value(64 * 1024),
        ..Default::default()
    };
    let effective = rohi_config::materialize::<LoggingConfig>(&Default::default(), &old);
    assert_eq!(effective.retention_bytes, 64 * 1024);
    assert_eq!(Config::validate(&effective), Err(FieldError::Invalid));

    let both = LoggingOverrides {
        segment_bytes: Override::Value(8 * 1024),
        ..old
    };
    let effective = rohi_config::materialize::<LoggingConfig>(&Default::default(), &both);
    assert_eq!(Config::validate(&effective), Ok(()));
    assert_eq!(effective.retention_bytes, 64 * 1024);
}

#[test]
fn the_schema_is_well_formed() {
    assert!(LoggingConfig::FIELDS.len() <= MAX_CONFIG_FIELDS);
    let mut ids: std::vec::Vec<_> = LoggingConfig::FIELDS.iter().map(|f| f.field.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, LoggingOverrides::FIELD_IDS);
    for field in LoggingConfig::FIELDS {
        assert!(field.field.name.len() <= 24, "{}", field.field.name);
    }
    let unit = |id| {
        LoggingConfig::FIELDS
            .iter()
            .find(|f| f.field.id == id)
            .unwrap()
            .field
            .unit
    };
    assert_eq!(unit(RETENTION), Unit::Bytes);
    assert_eq!(unit(SEGMENT), Unit::Bytes);
    assert_eq!(unit(FLUSH), Unit::Ms);
    assert_eq!(LoggingConfig::FIELDS[5].field.name, "flush_interval");
}

#[test]
fn the_level_enum_matches_the_type() {
    let variants = LEVELS[0].variants;
    assert_eq!(variants.len(), 5);
    for variant in variants {
        let level = LogLevel::from_u8(variant.value as u8).unwrap();
        assert_eq!(level.name(), variant.name);
    }
}

#[test]
fn the_persisted_section_is_pinned() {
    assert_eq!(LoggingOverrides::ID, SectionId::from_name("LoggingConfig"));
    assert_eq!(<LoggingOverrides as ConfigSection>::VERSION, 1);

    let overrides = LoggingOverrides {
        level: Override::Value(LogLevel::Debug),
        storage: Override::Value(LogStorageKind::Sd),
        compression: Override::Value(Compression::None),
        retention_bytes: Override::Value(65_536),
        segment_bytes: Override::Default,
        flush_interval_ms: Override::Value(5_000),
    };
    let mut buffer = [0u8; 64];
    let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
    // map(5): 1: 4, 2: 1, 3: 0, 4: 65536, 6: 5000
    assert_eq!(
        &buffer[..used],
        [
            0xa5, 0x01, 0x04, 0x02, 0x01, 0x03, 0x00, 0x04, 0x1a, 0x00, 0x01, 0x00, 0x00, 0x06,
            0x19, 0x13, 0x88
        ]
    );
    let decoded: LoggingOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
    assert_eq!(decoded, overrides);
}

#[test]
fn the_three_enums_are_discoverable_and_match_the_types() {
    assert_eq!(CONFIG_ENUMS.len(), 3);
    assert_eq!(CONFIG_ENUMS.len(), LoggingConfig::ENUMS.len());
    let ids: std::vec::Vec<_> = CONFIG_ENUMS.iter().map(|e| e.id).collect();
    assert_eq!(ids, [LEVEL_ENUM, STORAGE_ENUM, COMPRESSION_ENUM]);
    assert_eq!([LEVEL_ENUM, STORAGE_ENUM, COMPRESSION_ENUM], [1, 2, 3]);

    let enum_of = |field: u32| {
        LoggingConfig::FIELDS
            .iter()
            .find(|f| f.field.id == field)
            .unwrap()
            .field
            .enum_id
    };
    assert_eq!(enum_of(LEVEL), LEVEL_ENUM);
    assert_eq!(enum_of(STORAGE), STORAGE_ENUM);
    assert_eq!(enum_of(COMPRESSION), COMPRESSION_ENUM);

    let names = |id: u32| -> std::vec::Vec<(u32, &str)> {
        CONFIG_ENUMS
            .iter()
            .find(|e| e.id == id)
            .unwrap()
            .variants
            .iter()
            .map(|v| (v.value, v.name))
            .collect()
    };
    assert_eq!(names(STORAGE_ENUM), [(0, "internal"), (1, "sd")]);
    assert_eq!(names(COMPRESSION_ENUM), [(0, "none"), (1, "lz4")]);
    for (value, name) in names(STORAGE_ENUM) {
        let kind = LogStorageKind::from_variant(value).unwrap();
        assert_eq!((kind.variant(), kind.name()), (value, name));
    }
    for (value, name) in names(COMPRESSION_ENUM) {
        let codec = Compression::from_variant(value).unwrap();
        assert_eq!((codec.variant(), codec.name()), (value, name));
    }
    assert_eq!(LogStorageKind::from_variant(2), None);
    assert_eq!(Compression::from_variant(2), None);
}

#[test]
fn overrides_persisted_before_the_enums_still_decode() {
    // Written when `storage` and `compression` were text fields: the stored
    // form was always the CBOR index of the Rust enum, which is unchanged.
    let stored = [
        0xa5, 0x01, 0x04, 0x02, 0x01, 0x03, 0x00, 0x04, 0x1a, 0x00, 0x01, 0x00, 0x00, 0x06, 0x19,
        0x13, 0x88,
    ];
    let decoded: LoggingOverrides = rohi_config::decode_exact(&stored).unwrap();
    assert_eq!(decoded.storage, Override::Value(LogStorageKind::Sd));
    assert_eq!(decoded.compression, Override::Value(Compression::None));

    let effective = rohi_config::materialize::<LoggingConfig>(&Default::default(), &decoded);
    assert_eq!(effective.read_field(STORAGE), Ok(Some(Value::Enum(1))));
    assert_eq!(effective.read_field(COMPRESSION), Ok(Some(Value::Enum(0))));

    // Re-encoding what was just decoded gives the same bytes.
    let mut buffer = [0u8; 64];
    let used = rohi_config::encode_to_slice(&decoded, &mut buffer).unwrap();
    assert_eq!(&buffer[..used], stored);
}
