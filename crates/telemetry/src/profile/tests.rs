use super::*;
use crate::catalog::{BMX280_TEMPERATURE, LOCATION, SDS011_PM10, all_cataloged};

struct Caps;

impl FormatCapabilities for Caps {
    fn supported(&self, _: PacketFormatId) -> Selection {
        all_cataloged()
    }

    fn accepts_recipient(&self, _: PacketFormatId, recipient: &RecipientAddress) -> bool {
        recipient.0 != [0; 32]
    }
}

type Config = TelemetryConfig<3, 2>;

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
fn a_new_config_has_no_profile_and_a_new_profile_discloses_nothing() {
    let mut config = config();
    assert!(config.profiles.is_empty());
    config
        .add_profile("community", PacketFormatId::Robonomics)
        .unwrap();
    let profile = config.get("community").unwrap();
    assert!(profile.public.measurements.is_none());
    assert!(profile.private.is_empty());
}

#[test]
fn ids_are_validated_and_unique_and_the_table_is_bounded() {
    let mut config = config();
    for bad in ["", "Upper", "has space", "a.b", "abcdefghijklmnopqrstuvwxy"] {
        assert_eq!(
            config.add_profile(bad, PacketFormatId::Robonomics),
            Err(ProfileError::InvalidId),
            "{bad}"
        );
    }
    config.add_profile("a", PacketFormatId::Robonomics).unwrap();
    assert_eq!(
        config.add_profile("a", PacketFormatId::Robonomics),
        Err(ProfileError::Duplicate)
    );
    config.add_profile("b", PacketFormatId::Robonomics).unwrap();
    config.add_profile("c", PacketFormatId::Robonomics).unwrap();
    assert_eq!(
        config.add_profile("d", PacketFormatId::Robonomics),
        Err(ProfileError::TooManyProfiles)
    );
}

#[test]
fn set_public_replaces_and_rejects_unsupported_bits() {
    let mut config = config();
    config.add_profile("p", PacketFormatId::Robonomics).unwrap();
    config
        .set_public("p", only(BMX280_TEMPERATURE), &Caps)
        .unwrap();
    config.set_public("p", only(SDS011_PM10), &Caps).unwrap();
    let public = config.get("p").unwrap().public.measurements;
    assert!(public.contains(SDS011_PM10) && !public.contains(BMX280_TEMPERATURE));

    let before = config.clone();
    assert_eq!(
        config.set_public("p", only(crate::MeasurementId(18)), &Caps),
        Err(ProfileError::Selection(SelectionError::Unsupported(
            crate::MeasurementId(18)
        )))
    );
    assert_eq!(
        config, before,
        "a refused edit changes nothing, not even the generation"
    );
    assert_eq!(
        config.set_public("x", DataSelection::NONE, &Caps),
        Err(ProfileError::NotFound)
    );
}

#[test]
fn add_private_upserts_and_remove_private_removes() {
    let mut config = config();
    config
        .add_profile("owner", PacketFormatId::Robonomics)
        .unwrap();
    config
        .add_private("owner", key(1), only(LOCATION), &Caps)
        .unwrap();
    config
        .add_private("owner", key(2), only(SDS011_PM10), &Caps)
        .unwrap();
    // Replace, never union.
    config
        .add_private("owner", key(1), only(BMX280_TEMPERATURE), &Caps)
        .unwrap();
    let profile = config.get("owner").unwrap();
    assert_eq!(profile.private.len(), 2);
    let selection = profile.private[0].selection.measurements;
    assert!(selection.contains(BMX280_TEMPERATURE) && !selection.contains(LOCATION));

    assert_eq!(
        config.add_private("owner", key(3), only(LOCATION), &Caps),
        Err(ProfileError::TooManyRecipients)
    );
    // Replacing an existing recipient is allowed when the table is full.
    config
        .add_private("owner", key(2), only(LOCATION), &Caps)
        .unwrap();

    config.remove_private("owner", &key(1)).unwrap();
    assert_eq!(config.get("owner").unwrap().private.len(), 1);
    assert_eq!(
        config.remove_private("owner", &key(1)),
        Err(ProfileError::RecipientNotFound)
    );
}

#[test]
fn an_empty_private_selection_and_a_bad_key_are_refused() {
    let mut config = config();
    config
        .add_profile("owner", PacketFormatId::Robonomics)
        .unwrap();
    assert_eq!(
        config.add_private("owner", key(1), DataSelection::NONE, &Caps),
        Err(ProfileError::EmptyPrivateSelection)
    );
    assert_eq!(
        config.add_private("owner", RecipientAddress([0; 32]), only(LOCATION), &Caps),
        Err(ProfileError::InvalidRecipient)
    );
}

#[test]
fn location_is_never_public_by_default() {
    let mut config = config();
    config.add_profile("p", PacketFormatId::Robonomics).unwrap();
    config
        .set_public("p", DataSelection::local(Selection::ALL), &Caps)
        .unwrap();
    let public = config.get("p").unwrap().public.measurements;
    assert!(public.is_all());
    assert!(!public.resolve(&all_cataloged()).contains(LOCATION));
}

#[test]
fn revisions_and_incarnations_never_repeat() {
    let mut config = config();
    config.add_profile("p", PacketFormatId::Robonomics).unwrap();
    let first = config.get("p").unwrap().clone();
    config.set_public("p", only(SDS011_PM10), &Caps).unwrap();
    let edited = config.get("p").unwrap().clone();
    assert_eq!(edited.incarnation, first.incarnation);
    assert!(edited.revision > first.revision);

    config.remove_profile("p").unwrap();
    config.add_profile("p", PacketFormatId::Robonomics).unwrap();
    let recreated = config.get("p").unwrap();
    assert!(recreated.incarnation > edited.revision);
    assert_ne!(recreated.incarnation, first.incarnation);
    assert!(recreated.public.measurements.is_none());
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
    let mut config = config();
    for id in [
        "aaaaaaaaaaaaaaaaaaaaaaaa",
        "bbbbbbbbbbbbbbbbbbbbbbbb",
        "cccccccccccccccccccccccc",
    ] {
        config.add_profile(id, PacketFormatId::Robonomics).unwrap();
        config
            .set_public(
                id,
                DataSelection::local(Selection::NONE.with(SDS011_PM10)),
                &Caps,
            )
            .unwrap();
        for byte in [1, 2] {
            config
                .add_private(id, key(byte), only(LOCATION), &Caps)
                .unwrap();
        }
    }
    assert_eq!(round_trip(&config), config);
    // The persistent section payload is limited to 1024 bytes.
    assert!(minicbor::len(&config) <= 1024, "{}", minicbor::len(&config));
    assert!(config.validate(&Caps).is_ok());
}

#[test]
fn a_decoded_config_with_a_broken_policy_fails_validation() {
    let mut config = config();
    config.add_profile("p", PacketFormatId::Robonomics).unwrap();
    config
        .add_private("p", key(1), only(LOCATION), &Caps)
        .unwrap();

    let mut broken = config.clone();
    broken.profiles[0].private[0].selection = DataSelection::NONE;
    assert_eq!(
        broken.validate(&Caps),
        Err(ProfileError::EmptyPrivateSelection)
    );

    let mut future = config.clone();
    future.profiles[0].revision = future.generation + 1;
    assert!(future.validate(&Caps).is_err());

    let mut duplicate = config.clone();
    let copy = duplicate.profiles[0].clone();
    duplicate.profiles.push(copy).unwrap();
    assert_eq!(duplicate.validate(&Caps), Err(ProfileError::Duplicate));
}

#[test]
fn too_many_elements_do_not_decode() {
    let mut big = TelemetryConfig::<4, 2>::default();
    for id in ["a", "b", "c", "d"] {
        big.add_profile(id, PacketFormatId::Robonomics).unwrap();
    }
    let mut buf = [0u8; 512];
    let len = {
        let mut cursor = minicbor::encode::write::Cursor::new(&mut buf[..]);
        minicbor::encode(&big, &mut cursor).unwrap();
        cursor.position()
    };
    assert!(minicbor::decode::<Config>(&buf[..len]).is_err());
}
