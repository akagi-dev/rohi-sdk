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
use rohi_config::ConfigSection;
use rohi_service::{
    ConfigField, FieldError, ManagedConfig, ManagedService, ServiceRegistry, ServiceState, Tracked,
    Value,
};

use crate::*;

fn config() -> LocationConfig {
    LocationConfig {
        latitude: 52.52,
        longitude: 13.405,
        height_m: Some(34.0),
    }
}

#[test]
fn cbor_encoding_is_stable() {
    let mut buffer = [0u8; 64];
    let used = rohi_config::encode_to_slice(&config(), &mut buffer).unwrap();
    // array(3), then three float64 (0xfb + 8 bytes).
    assert_eq!(used, 1 + 3 * 9);
    assert_eq!(buffer[0], 0x83);
    assert_eq!(buffer[1], 0xfb);
    assert_eq!(&buffer[2..10], &52.52f64.to_be_bytes());
    assert_eq!(&buffer[11..19], &13.405f64.to_be_bytes());
    assert_eq!(&buffer[20..28], &34.0f64.to_be_bytes());

    // Without a height the trailing element is omitted.
    let open = LocationConfig {
        height_m: None,
        ..config()
    };
    let used = rohi_config::encode_to_slice(&open, &mut buffer).unwrap();
    assert_eq!(buffer[0], 0x82);
    assert_eq!(used, 1 + 2 * 9);
    let decoded: LocationConfig = rohi_config::decode_exact(&buffer[..used]).unwrap();
    assert_eq!(decoded, open);
}

#[test]
fn section_id_and_version() {
    // The name of the struct is part of the persistent ABI.
    assert_eq!(
        LocationConfig::ID,
        rohi_config::SectionId::from_name("LocationConfig")
    );
    assert_eq!(
        LocationConfig::ID,
        rohi_config::SectionId(0xce09_8b4a_3e38_1f40)
    );
    assert_eq!(<LocationConfig as ConfigSection>::VERSION, 1);
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
    let mut section = LocationConfig::defaults();
    section
        .set_field(field::HEIGHT_M, Value::F64(10.0))
        .unwrap();
    assert_eq!(ManagedConfig::validate(&section), Err(FieldError::Invalid));
    section.set_field(field::LATITUDE, Value::F64(1.5)).unwrap();
    assert_eq!(ManagedConfig::validate(&section), Err(FieldError::Invalid));
    section
        .set_field(field::LONGITUDE, Value::F64(2.5))
        .unwrap();
    assert_eq!(ManagedConfig::validate(&section), Ok(()));
    assert_eq!(
        section.read_field(field::LATITUDE),
        Ok(Some(Value::F64(1.5)))
    );
    section.unset_field(field::HEIGHT_M).unwrap();
    assert_eq!(section.read_field(field::HEIGHT_M), Ok(None));
    assert_eq!(
        section.unset_field(field::LATITUDE),
        Err(FieldError::NotOptional)
    );
    assert_eq!(
        section.set_field(field::LATITUDE, Value::U64(1)),
        Err(FieldError::WrongKind)
    );
}

#[test]
fn the_schema_bounds_match_the_validation() {
    let field = |id| -> &ConfigField {
        LocationConfig::FIELDS
            .iter()
            .find(|f| f.field.id == id)
            .unwrap()
    };
    assert_eq!((field(1).min, field(1).max), (Some(-90.0), Some(90.0)));
    assert_eq!((field(2).min, field(2).max), (Some(-180.0), Some(180.0)));
    assert!(field(3).optional && !field(1).optional);
    assert_eq!(field(1).field.unit, "deg");
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
    let status = block_on(registry.status("location", 0)).unwrap();
    assert_eq!(status.summary.state, ServiceState::Disabled);
    assert!(!status.summary.ready);
    assert_eq!(status.issues[0].code, "not_set");
    assert!(status.fields.is_empty());

    shared.publish_configured(Some(&config()));
    let status = block_on(registry.status("location", 1_000)).unwrap();
    assert_eq!(status.summary.state, ServiceState::Running);
    assert!(status.summary.ready);
    assert!(status.issues.is_empty());
    assert_eq!(
        status.fields[0],
        (status_field::LATITUDE, Value::F64(52.52))
    );
    assert_eq!(status.fields[2], (status_field::HEIGHT_M, Value::F64(34.0)));
    assert_eq!(
        status.fields[3],
        (status_field::SOURCE, Value::variant("configured").unwrap())
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
