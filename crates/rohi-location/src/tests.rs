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
use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_config::{ConfigOverrides, ConfigSection, Override};
use rohi_service::{
    Bounds, ConfigField, FieldError, ManagedConfig, ManagedService, ServiceRegistry, ServiceState,
    Tracked, Unit, Value,
};

use crate::*;

fn config() -> LocationConfig {
    LocationConfig {
        latitude: 52.52,
        longitude: 13.405,
        height_m: Some(34.0),
    }
}

fn overrides() -> LocationOverrides {
    LocationOverrides {
        latitude: Override::Value(52.52),
        longitude: Override::Value(13.405),
        height_m: Override::Value(Some(34.0)),
    }
}

#[test]
fn cbor_encoding_is_stable() {
    let mut buffer = [0u8; 64];
    let used = rohi_config::encode_to_slice(&overrides(), &mut buffer).unwrap();
    // map(3): key, then float64 (0xfb + 8 bytes), three times.
    assert_eq!(used, 1 + 3 * 10);
    assert_eq!(buffer[0], 0xa3);
    assert_eq!(&buffer[1..3], [0x01, 0xfb]);
    assert_eq!(&buffer[3..11], &52.52f64.to_be_bytes());
    assert_eq!(&buffer[11..13], [0x02, 0xfb]);
    assert_eq!(&buffer[13..21], &13.405f64.to_be_bytes());
    assert_eq!(&buffer[21..23], [0x03, 0xfb]);
    assert_eq!(&buffer[23..31], &34.0f64.to_be_bytes());
    let decoded: LocationOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
    assert_eq!(decoded, overrides());
    assert_eq!(decoded.over_default(), config());

    // Without a height override the entry is omitted.
    let open = LocationOverrides {
        height_m: Override::Default,
        ..overrides()
    };
    let used = rohi_config::encode_to_slice(&open, &mut buffer).unwrap();
    assert_eq!(buffer[0], 0xa2);
    assert_eq!(used, 1 + 2 * 10);
    let decoded: LocationOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
    assert_eq!(decoded, open);
    assert_eq!(decoded.over_default().height_m, None);
}

#[test]
fn section_id_and_version() {
    // The name of the section type is part of the persistent ABI.
    assert_eq!(
        LocationOverrides::ID,
        rohi_config::SectionId::from_name("LocationConfig")
    );
    assert_eq!(
        LocationOverrides::ID,
        rohi_config::SectionId(0xce09_8b4a_3e38_1f40)
    );
    assert_eq!(<LocationOverrides as ConfigSection>::VERSION, 1);
}

#[test]
fn validation_rejects_what_is_not_a_position() {
    assert!(config().is_valid());
    for bad in [
        LocationConfig {
            latitude: 90.1,
            ..config()
        },
        LocationConfig {
            latitude: -90.1,
            ..config()
        },
        LocationConfig {
            longitude: 180.1,
            ..config()
        },
        LocationConfig {
            longitude: f64::NAN,
            ..config()
        },
        LocationConfig {
            latitude: f64::INFINITY,
            ..config()
        },
        LocationConfig {
            height_m: Some(1.0e6),
            ..config()
        },
        LocationConfig {
            height_m: Some(f64::NAN),
            ..config()
        },
    ] {
        assert!(!bad.is_valid(), "{bad:?}");
        assert_eq!(ManagedConfig::validate(&bad), Err(FieldError::Invalid));
    }
    // The poles and the antimeridian are valid.
    let edge = LocationConfig {
        latitude: -90.0,
        longitude: 180.0,
        height_m: None,
    };
    assert!(edge.is_valid());
}

#[test]
fn a_patch_needs_both_coordinates() {
    // A first `set height_m=...` must not store "0, 0".
    let mut section = LocationConfig::default();
    section
        .set_field(field::HEIGHT_M, Value::F32(10.0))
        .unwrap();
    assert_eq!(ManagedConfig::validate(&section), Err(FieldError::Invalid));
    section.set_field(field::LATITUDE, Value::F32(1.5)).unwrap();
    assert_eq!(ManagedConfig::validate(&section), Err(FieldError::Invalid));
    section
        .set_field(field::LONGITUDE, Value::F32(2.5))
        .unwrap();
    assert_eq!(ManagedConfig::validate(&section), Ok(()));
    assert_eq!(
        section.read_field(field::LATITUDE),
        Ok(Some(Value::F32(1.5)))
    );
    let mut overrides = LocationOverrides::default();
    overrides.capture(&section, field::HEIGHT_M);
    overrides.clear(field::HEIGHT_M);
    assert_eq!(
        overrides.over_default().read_field(field::HEIGHT_M),
        Ok(None)
    );
    assert_eq!(
        section.set_field(field::LATITUDE, Value::U64(1)),
        Err(FieldError::WrongKind)
    );
}

#[test]
fn the_default_is_no_position_and_reads_as_unset() {
    let defaults = LocationConfig::default();
    assert!(!defaults.is_valid());
    assert_eq!(defaults.read_field(field::LATITUDE), Ok(None));
    assert_eq!(defaults.read_field(field::LONGITUDE), Ok(None));
    assert_eq!(defaults.read_field(field::HEIGHT_M), Ok(None));
    assert!(LocationOverrides::default().is_empty());
}

#[test]
fn an_unconfigured_section_is_valid_but_half_a_position_is_not() {
    let mut section = LocationConfig::default();
    assert!(section.is_unset());
    assert_eq!(ManagedConfig::validate(&section), Ok(()));
    section
        .set_field(field::LATITUDE, Value::F32(52.5))
        .unwrap();
    assert!(!section.is_unset());
    assert_eq!(ManagedConfig::validate(&section), Err(FieldError::Invalid));
    section
        .set_field(field::LONGITUDE, Value::F32(13.4))
        .unwrap();
    assert_eq!(ManagedConfig::validate(&section), Ok(()));
}

#[test]
fn none_applies_to_the_height_only() {
    let mut section = config();
    section
        .set_field(field::HEIGHT_M, Value::F32(34.0))
        .unwrap();
    assert_eq!(section.set_none(field::HEIGHT_M), Ok(()));
    assert_eq!(section.height_m, None);
    assert_eq!(
        section.set_none(field::LATITUDE),
        Err(FieldError::NotOptional)
    );
    assert_eq!(
        section.set_none(field::LONGITUDE),
        Err(FieldError::NotOptional)
    );
    assert_eq!(section.set_none(9), Err(FieldError::UnknownField));
}

#[test]
fn every_field_can_be_overridden() {
    let mut ids: std::vec::Vec<_> = LocationConfig::FIELDS.iter().map(|f| f.field.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, LocationOverrides::FIELD_IDS);
}

#[test]
fn the_schema_bounds_match_the_validation() {
    let field = |id| -> &ConfigField {
        LocationConfig::FIELDS
            .iter()
            .find(|f| f.field.id == id)
            .unwrap()
    };
    assert_eq!(field(1).bounds, Some(Bounds::F32(-90.0, 90.0)));
    assert_eq!(field(2).bounds, Some(Bounds::F32(-180.0, 180.0)));
    assert!(field(3).optional && !field(1).optional);
    assert_eq!(field(1).field.unit, Unit::Degrees);
}

#[test]
fn nothing_is_known_until_a_location_is_published() {
    let shared = LocationShared::<NoopRawMutex>::new();
    assert_eq!(shared.location(), None);
    shared.publish_configured(Some(&config()));
    let location = shared.location().unwrap();
    assert_eq!(location.latitude, 52.52);
    assert_eq!(location.source, LocationSourceKind::Configured);
    shared.publish_configured(None);
    assert_eq!(shared.location(), None);
}

#[test]
fn an_invalid_section_is_treated_as_no_location() {
    let shared = LocationShared::<NoopRawMutex>::new();
    shared.publish_configured(Some(&LocationConfig {
        latitude: 200.0,
        ..config()
    }));
    assert_eq!(shared.location(), None);
}

#[test]
fn a_live_fix_beats_the_configured_location_and_gives_way_when_lost() {
    let shared = LocationShared::<NoopRawMutex>::new();
    shared.publish_configured(Some(&config()));
    let fix = Location {
        latitude: 52.53,
        longitude: 13.41,
        height_m: None,
        // Whatever the publisher says, a fix is a GPS location.
        source: LocationSourceKind::Configured,
    };
    shared.publish_fix(Some(fix));
    let current = shared.current().unwrap();
    assert_eq!(current.latitude, 52.53);
    assert_eq!(current.source, LocationSourceKind::Gps);
    shared.publish_fix(None);
    assert_eq!(
        shared.current().unwrap().source,
        LocationSourceKind::Configured
    );
    // A fix alone works without any configuration.
    shared.publish_configured(None);
    shared.publish_fix(Some(fix));
    assert!(shared.current().is_some());
}

#[test]
fn the_service_is_disabled_until_a_location_is_set() {
    let shared = LocationShared::<NoopRawMutex>::new();
    let registry = (Tracked::<_, NoopRawMutex>::new(LocationService(&shared)),);
    let status = block_on(registry.status(SERVICE_ID, 0)).unwrap();
    assert_eq!(status.summary.state, ServiceState::Disabled);
    assert!(!status.summary.ready);
    assert_eq!(status.issues[0].code, issue::NOT_SET);
    assert!(status.fields.is_empty());

    shared.publish_configured(Some(&config()));
    let status = block_on(registry.status(SERVICE_ID, 1_000)).unwrap();
    assert_eq!(status.summary.state, ServiceState::Running);
    assert!(status.summary.ready);
    assert!(status.issues.is_empty());
    assert_eq!(
        status.fields[0],
        (status_field::LATITUDE, Value::F32(52.52))
    );
    assert_eq!(status.fields[2], (status_field::HEIGHT_M, Value::F32(34.0)));
    assert_eq!(
        status.fields[3],
        (status_field::SOURCE, Value::Enum(source::CONFIGURED))
    );
    // Setting the location is a change an observer wants to see.
    assert!(status.summary.revision > 1);
}

#[test]
fn the_status_schema_fits_the_protocol() {
    type S = LocationService<'static, NoopRawMutex>;
    assert_eq!(<S as ManagedService>::STATUS_SCHEMA.len(), 4);
    const { assert!(!<S as ManagedService>::ESSENTIAL) };
    assert!(
        <S as ManagedService>::STATUS_SCHEMA
            .iter()
            .all(|field| field.name.len() <= 24)
    );
}
