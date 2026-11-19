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
use super::*;
use crate::catalog::{BMX280_TEMPERATURE, LOCATION, SDS011_PM10, all_cataloged};

struct Caps;

impl FormatCapabilities for Caps {
    fn supported(&self) -> Selection {
        all_cataloged()
    }

    fn accepts_recipient(&self, recipient: &RecipientAddress) -> bool {
        recipient.0 != [0; 32]
    }
}

type Config = TelemetryConfig<2>;

fn key(byte: u8) -> RecipientAddress {
    RecipientAddress([byte; 32])
}

fn only(id: crate::MeasurementId) -> DataSelection {
    DataSelection::local(Selection::NONE.with(id))
}

fn config() -> Config {
    Config::default()
}

#[test]
fn a_new_config_has_two_empty_slots_that_disclose_nothing() {
    let config = config();
    assert_eq!(config.profiles.len(), 2);
    assert_eq!(config.generation, 0);
    for index in 0..2 {
        let profile = config.get(index).unwrap();
        assert!(profile.public.measurements.is_none());
        assert!(profile.private.is_none());
        assert_eq!(profile.revision, 0);
    }
    assert!(config.validate(&Caps).is_ok());
}

#[test]
fn the_slot_index_is_bounded_and_a_refused_index_changes_nothing() {
    let mut config = config();
    let before = config.clone();
    for index in [2, 3, u8::MAX] {
        assert!(config.get(index).is_none());
        assert_eq!(
            config.set_public(index, only(SDS011_PM10), &Caps),
            Err(ProfileError::InvalidIndex)
        );
        assert_eq!(
            config.set_private(index, key(1), only(LOCATION), &Caps),
            Err(ProfileError::InvalidIndex)
        );
    }
    assert_eq!(config, before, "not even the generation moved");
    // Both boundary slots work.
    config.set_public(0, only(SDS011_PM10), &Caps).unwrap();
    config.set_public(1, only(SDS011_PM10), &Caps).unwrap();
}

#[test]
fn set_public_replaces_rejects_unsupported_bits_and_leaves_the_other_slot_alone() {
    let mut config = config();
    config
        .set_public(0, only(BMX280_TEMPERATURE), &Caps)
        .unwrap();
    let other = *config.get(1).unwrap();
    config.set_public(0, only(SDS011_PM10), &Caps).unwrap();
    let public = config.get(0).unwrap().public.measurements;
    assert!(public.contains(SDS011_PM10) && !public.contains(BMX280_TEMPERATURE));
    assert_eq!(*config.get(1).unwrap(), other, "slot 1 is not touched");

    let before = config.clone();
    assert_eq!(
        config.set_public(0, only(crate::MeasurementId(18)), &Caps),
        Err(ProfileError::Selection(SelectionError::Unsupported(
            crate::MeasurementId(18)
        )))
    );
    assert_eq!(config, before);
}

#[test]
fn set_private_replaces_the_only_recipient_and_none_removes_it() {
    let mut config = config();
    config
        .set_private(1, key(1), only(LOCATION), &Caps)
        .unwrap();
    // A second recipient replaces the first: there is only one.
    config
        .set_private(1, key(2), only(SDS011_PM10), &Caps)
        .unwrap();
    let section = config.get(1).unwrap().private.unwrap();
    assert_eq!(section.recipient, key(2));
    assert!(section.selection.measurements.contains(SDS011_PM10));
    assert!(!section.selection.measurements.contains(LOCATION));
    assert!(config.get(0).unwrap().private.is_none());

    let revision = config.get(1).unwrap().revision;
    // `none` removes it; the recipient is not looked at.
    config
        .set_private(1, RecipientAddress([0; 32]), DataSelection::NONE, &Caps)
        .unwrap();
    let profile = config.get(1).unwrap();
    assert!(profile.private.is_none());
    assert!(profile.revision > revision);
    assert!(config.validate(&Caps).is_ok());
}

#[test]
fn a_bad_key_or_unsupported_private_bit_is_refused() {
    let mut config = config();
    let before = config.clone();
    assert_eq!(
        config.set_private(0, RecipientAddress([0; 32]), only(LOCATION), &Caps),
        Err(ProfileError::InvalidRecipient)
    );
    assert_eq!(
        config.set_private(0, key(1), only(crate::MeasurementId(40)), &Caps),
        Err(ProfileError::Selection(SelectionError::Unsupported(
            crate::MeasurementId(40)
        )))
    );
    assert_eq!(config, before);
}

#[test]
fn location_is_never_public_by_default() {
    let mut config = config();
    config
        .set_public(0, DataSelection::local(Selection::ALL), &Caps)
        .unwrap();
    let public = config.get(0).unwrap().public.measurements;
    assert!(public.is_all());
    assert!(!public.resolve(&all_cataloged()).contains(LOCATION));
}

#[test]
fn revisions_never_repeat_and_only_the_edited_slot_changes() {
    let mut config = config();
    config.set_public(0, only(SDS011_PM10), &Caps).unwrap();
    let first = config.get(0).unwrap().revision;
    config.set_public(0, only(SDS011_PM10), &Caps).unwrap();
    let second = config.get(0).unwrap().revision;
    assert!(second > first, "even an identical edit is a new revision");
    assert_eq!(config.get(1).unwrap().revision, 0);
    config
        .set_private(1, key(1), only(LOCATION), &Caps)
        .unwrap();
    assert!(config.get(1).unwrap().revision > second);
    assert_eq!(config.get(0).unwrap().revision, second);
}

#[test]
fn clear_empties_every_slot_and_gives_every_slot_a_new_revision() {
    let mut config = config();
    config.set_public(0, only(SDS011_PM10), &Caps).unwrap();
    config
        .set_private(1, key(1), only(LOCATION), &Caps)
        .unwrap();
    let revisions = [
        config.get(0).unwrap().revision,
        config.get(1).unwrap().revision,
    ];
    config.clear();
    for (index, old) in revisions.into_iter().enumerate() {
        let profile = config.get(index as u8).unwrap();
        assert!(profile.public.measurements.is_none() && profile.private.is_none());
        assert!(profile.revision > old, "a reset invalidates old packets");
        assert!(profile.revision <= config.generation);
    }
    assert!(config.validate(&Caps).is_ok());
}

fn round_trip(config: &Config) -> Config {
    let mut buf = [0u8; 1024];
    let len = {
        let mut cursor = minicbor::encode::write::Cursor::new(&mut buf[..]);
        minicbor::encode(config, &mut cursor).unwrap();
        cursor.position()
    };
    assert_eq!(minicbor::len(config), len);
    minicbor::decode(&buf[..len]).unwrap()
}

#[test]
fn the_config_round_trips_and_a_full_one_fits_the_section_budget() {
    assert_eq!(round_trip(&config()), config());
    let mut config = config();
    for index in 0..2 {
        config
            .set_public(index, DataSelection::local(Selection::ALL), &Caps)
            .unwrap();
        config
            .set_private(
                index,
                key(index + 1),
                DataSelection::local(Selection::ALL),
                &Caps,
            )
            .unwrap();
    }
    assert_eq!(round_trip(&config), config);
    // The persistent section payload is limited to 1024 bytes.
    assert!(minicbor::len(&config) <= 1024, "{}", minicbor::len(&config));
    assert!(config.validate(&Caps).is_ok());
}

#[test]
fn a_decoded_config_with_a_broken_policy_fails_validation() {
    let mut config = config();
    config
        .set_private(0, key(1), only(LOCATION), &Caps)
        .unwrap();

    let mut empty_private = config.clone();
    if let Some(section) = &mut empty_private.profiles[0].private {
        section.selection = DataSelection::NONE;
    }
    assert_eq!(
        empty_private.validate(&Caps),
        Err(ProfileError::EmptyPrivateSelection)
    );

    let mut future = config.clone();
    future.profiles[1].revision = future.generation + 1;
    assert_eq!(
        future.validate(&Caps),
        Err(ProfileError::InconsistentGeneration)
    );

    let mut bad_key = config.clone();
    bad_key.profiles[0].private = Some(PrivateSectionPolicy {
        recipient: RecipientAddress([0; 32]),
        selection: only(LOCATION),
    });
    assert_eq!(bad_key.validate(&Caps), Err(ProfileError::InvalidRecipient));

    let mut unsupported = config;
    unsupported.profiles[1].public = only(crate::MeasurementId(18));
    assert!(matches!(
        unsupported.validate(&Caps),
        Err(ProfileError::Selection(_))
    ));
}

#[test]
fn another_number_of_slots_does_not_decode() {
    let mut buf = [0u8; 512];
    for slots in [1usize, 3] {
        let len = match slots {
            1 => rohi_config::encode_to_slice(&TelemetryConfig::<1>::default(), &mut buf),
            _ => rohi_config::encode_to_slice(&TelemetryConfig::<3>::default(), &mut buf),
        }
        .unwrap();
        assert!(minicbor::decode::<Config>(&buf[..len]).is_err(), "{slots}");
    }
}
