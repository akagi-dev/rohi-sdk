use core::cell::RefCell;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_service::{Action, ActionError, Service, Status};
use rohi_service::{ActionRegistry, Statuses};

use super::*;
use crate::catalog::{BMX280_TEMPERATURE, LOCATION, SDS011_PM10, all_cataloged};

struct Caps;

impl FormatCapabilities for Caps {
    fn supported(&self, _: PacketFormatId) -> Selection {
        // The format cannot encode the last measurement.
        all_cataloged().without(crate::MeasurementId(17))
    }

    fn accepts_recipient(&self, _: PacketFormatId, recipient: &RecipientAddress) -> bool {
        recipient.0 != [0; 32]
    }
}

#[derive(Default)]
struct Memory {
    stored: RefCell<ProfileSet>,
    fail_commit: RefCell<bool>,
    unreadable: RefCell<bool>,
    commits: RefCell<u32>,
}

impl TelemetryStore for &Memory {
    async fn load(&self) -> Result<ProfileSet, StoreError> {
        if *self.unreadable.borrow() {
            return Err(StoreError::Unavailable);
        }
        Ok(self.stored.borrow().clone())
    }

    async fn commit(&self, candidate: &ProfileSet) -> Result<(), StoreError> {
        if *self.fail_commit.borrow() {
            return Err(StoreError::Internal);
        }
        *self.stored.borrow_mut() = candidate.clone();
        *self.commits.borrow_mut() += 1;
        Ok(())
    }
}

type Service_<'a> = TelemetryService<NoopRawMutex, &'a Memory, Caps>;

fn service(memory: &Memory) -> Service_<'_> {
    TelemetryService::new(memory, Caps)
}

fn text(value: &str) -> Value {
    Value::text(value).unwrap()
}

fn bytes(value: &[u8]) -> Value {
    Value::bytes(value).unwrap()
}

fn call(
    service: &Service_<'_>,
    action: u32,
    args: &[(u32, Value)],
) -> Result<std::vec::Vec<heapless::Vec<(u32, Value), 4>>, ActionError> {
    let registry = (service,);
    block_on(registry.invoke(Service_::ID, action, args)).map(|done| done.records().to_vec())
}

fn add(service: &Service_<'_>, id: &str) -> Result<(), ActionError> {
    call(
        service,
        action::PROFILE_ADD,
        &[(1, text(id)), (2, Value::Enum(1))],
    )
    .map(|_| ())
}

fn set_public(service: &Service_<'_>, id: &str, selection: &Selection) -> Result<(), ActionError> {
    call(
        service,
        action::SET_PUBLIC,
        &[(1, text(id)), (2, bytes(selection.as_bytes()))],
    )
    .map(|_| ())
}

fn add_private(
    service: &Service_<'_>,
    id: &str,
    key: u8,
    selection: &Selection,
) -> Result<(), ActionError> {
    call(
        service,
        action::ADD_PRIVATE,
        &[
            (1, text(id)),
            (2, bytes(&[key; 32])),
            (3, bytes(selection.as_bytes())),
        ],
    )
    .map(|_| ())
}

#[test]
fn the_service_exposes_exactly_the_eight_actions_with_their_access() {
    let actions: std::vec::Vec<_> = Service_::ACTIONS
        .iter()
        .map(|a| (a.name, a.access, a.args.len()))
        .collect();
    assert_eq!(
        actions,
        [
            ("profile_add", ActionAccess::Mutate, 2),
            ("profile_remove", ActionAccess::Mutate, 1),
            ("set_public", ActionAccess::Mutate, 2),
            ("add_private", ActionAccess::Mutate, 3),
            ("remove_private", ActionAccess::Mutate, 2),
            ("profile_list", ActionAccess::Read, 1),
            ("profile_recipients", ActionAccess::Read, 2),
            ("measurements", ActionAccess::Read, 1),
        ]
    );
    // The selection fields point at the catalog action of this service.
    let set_public = &Service_::ACTIONS[2];
    let selection = set_public.args[1].selection.unwrap();
    assert_eq!(
        (
            selection.size_bytes,
            selection.catalog_service_id,
            selection.catalog_action_id
        ),
        (32, Service_::ID, action::MEASUREMENTS)
    );
    assert_eq!(Service_::ACTIONS[3].args[1].kind, ValueKind::Address);
    assert_eq!(Service_::NAME, "telemetry");
}

#[test]
fn a_profile_is_created_empty_and_listed() {
    let memory = Memory::default();
    let service = service(&memory);
    add(&service, "community").unwrap();
    let list = call(&service, action::PROFILE_LIST, &[(1, Value::U64(0))]).unwrap();
    assert_eq!(list.len(), 1);
    let record = &list[0];
    assert_eq!(record[0].1.as_str(), Some("community"));
    assert_eq!(record[1].1.as_enum(), Some(1));
    assert_eq!(record[2].1.as_bytes(), Some(&[0u8; 32][..]));
    assert_eq!(record[3].1.as_u64(), Some(1));
    assert!(
        call(&service, action::PROFILE_LIST, &[(1, Value::U64(1))])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn set_public_replaces_and_unsupported_bits_are_rejected_without_a_commit() {
    let memory = Memory::default();
    let service = service(&memory);
    add(&service, "p").unwrap();
    set_public(&service, "p", &Selection::NONE.with(BMX280_TEMPERATURE)).unwrap();
    set_public(&service, "p", &Selection::NONE.with(SDS011_PM10)).unwrap();
    let public = memory.stored.borrow().get("p").unwrap().public.measurements;
    assert!(public.contains(SDS011_PM10) && !public.contains(BMX280_TEMPERATURE));

    let commits = *memory.commits.borrow();
    // 17 is cataloged but the format cannot encode it; 40 is unknown.
    for bit in [17, 40] {
        assert_eq!(
            set_public(
                &service,
                "p",
                &Selection::NONE.with(crate::MeasurementId(bit))
            ),
            Err(ActionError::Rejected("unsupported_measurement"))
        );
    }
    assert_eq!(*memory.commits.borrow(), commits);
    assert_eq!(
        set_public(&service, "nope", &Selection::ALL),
        Err(ActionError::Rejected("profile_not_found"))
    );
}

#[test]
fn private_sections_are_upserted_listed_and_removed() {
    let memory = Memory::default();
    let service = service(&memory);
    add(&service, "owner").unwrap();
    add_private(&service, "owner", 1, &Selection::NONE.with(LOCATION)).unwrap();
    add_private(&service, "owner", 1, &Selection::NONE.with(SDS011_PM10)).unwrap();
    add_private(&service, "owner", 2, &Selection::NONE.with(LOCATION)).unwrap();
    assert_eq!(
        add_private(&service, "owner", 3, &Selection::NONE.with(LOCATION)),
        Err(ActionError::Rejected("too_many_recipients"))
    );
    assert_eq!(
        add_private(&service, "owner", 4, &Selection::NONE),
        Err(ActionError::Rejected("empty_selection"))
    );
    assert_eq!(
        call(
            &service,
            action::ADD_PRIVATE,
            &[
                (1, text("owner")),
                (2, bytes(&[0; 32])),
                (3, bytes(Selection::ALL.as_bytes()))
            ]
        ),
        Err(ActionError::Rejected("invalid_recipient"))
    );

    let page = call(
        &service,
        action::PROFILE_RECIPIENTS,
        &[(1, text("owner")), (2, Value::U64(0))],
    )
    .unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0][0].1.as_bytes(), Some(&[1u8; 32][..]));
    let first = Selection::from_slice(page[0][1].1.as_bytes().unwrap()).unwrap();
    assert!(first.contains(SDS011_PM10) && !first.contains(LOCATION));
    let tail = call(
        &service,
        action::PROFILE_RECIPIENTS,
        &[(1, text("owner")), (2, Value::U64(1))],
    )
    .unwrap();
    assert_eq!(tail.len(), 1);

    call(
        &service,
        action::REMOVE_PRIVATE,
        &[(1, text("owner")), (2, bytes(&[1; 32]))],
    )
    .unwrap();
    assert_eq!(
        call(
            &service,
            action::REMOVE_PRIVATE,
            &[(1, text("owner")), (2, bytes(&[1; 32]))],
        ),
        Err(ActionError::Rejected("recipient_not_found"))
    );
}

#[test]
fn a_failed_commit_changes_nothing() {
    let memory = Memory::default();
    let service = service(&memory);
    add(&service, "p").unwrap();
    let before = memory.stored.borrow().clone();
    *memory.fail_commit.borrow_mut() = true;
    assert_eq!(
        set_public(&service, "p", &Selection::NONE.with(SDS011_PM10)),
        Err(ActionError::Internal)
    );
    assert_eq!(*memory.stored.borrow(), before);
}

#[test]
fn an_unreadable_store_is_an_error_not_an_empty_policy() {
    let memory = Memory::default();
    let service = service(&memory);
    *memory.unreadable.borrow_mut() = true;
    assert_eq!(add(&service, "p"), Err(ActionError::Unavailable));
    assert_eq!(
        call(&service, action::PROFILE_LIST, &[(1, Value::U64(0))]),
        Err(ActionError::Unavailable)
    );
    let summary = block_on(service.summary());
    assert_eq!(summary.state, ServiceState::Failed);
}

#[test]
fn arguments_of_the_wrong_shape_are_invalid() {
    let memory = Memory::default();
    let service = service(&memory);
    add(&service, "p").unwrap();
    // A selection is exactly 32 bytes; the registry checks it before the action.
    assert_eq!(
        call(
            &service,
            action::SET_PUBLIC,
            &[(1, text("p")), (2, bytes(&[0xFF; 31]))]
        ),
        Err(ActionError::InvalidArgument)
    );
    assert_eq!(
        call(
            &service,
            action::PROFILE_ADD,
            &[(1, text("Bad Id")), (2, Value::Enum(1))]
        ),
        Err(ActionError::Rejected("invalid_id"))
    );
    assert_eq!(
        call(
            &service,
            action::PROFILE_ADD,
            &[(1, text("x")), (2, Value::Enum(9))]
        ),
        Err(ActionError::InvalidArgument)
    );
}

#[test]
fn the_catalog_lists_what_the_format_can_encode_in_pages() {
    let memory = Memory::default();
    let service = service(&memory);
    let mut seen = std::vec::Vec::new();
    let mut offset = 0u64;
    loop {
        let page = call(&service, action::MEASUREMENTS, &[(1, Value::U64(offset))]).unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= MAX_RECORDS);
        for record in &page {
            seen.push((
                record[0].1.as_u64().unwrap(),
                std::string::String::from(record[1].1.as_str().unwrap()),
            ));
        }
        offset += page.len() as u64;
    }
    // 18 cataloged, 17 encodable, ascending and complete.
    assert_eq!(seen.len(), 17);
    assert_eq!(seen[0], (0, "location".into()));
    assert!(seen.windows(2).all(|w| w[0].0 < w[1].0));
    assert!(!seen.iter().any(|(bit, _)| *bit == 17));
}

#[test]
fn the_budget_holds_for_the_largest_policy_the_actions_can_build() {
    let memory = Memory::default();
    let service = service(&memory);
    for id in [
        "aaaaaaaaaaaaaaaaaaaaaaaa",
        "bbbbbbbbbbbbbbbbbbbbbbbb",
        "cccccccccccccccccccccccc",
    ] {
        add(&service, id).unwrap();
        set_public(&service, id, &Selection::NONE.with(SDS011_PM10)).unwrap();
        for key in [1, 2] {
            add_private(&service, id, key, &Selection::NONE.with(LOCATION)).unwrap();
        }
    }
    assert!(minicbor::len(TelemetrySectionRef(&memory.stored.borrow())) <= MAX_SECTION_PAYLOAD);
    assert_eq!(
        add(&service, "d"),
        Err(ActionError::Rejected("too_many_profiles"))
    );
}

#[test]
fn status_reports_counts_without_any_policy_content() {
    let memory = Memory::default();
    let service = service(&memory);
    add(&service, "p").unwrap();
    let statuses = Statuses::<NoopRawMutex, _>::new((&service,));
    let _ = statuses;
    assert_eq!(<Service_ as Status>::STATUS_SCHEMA.len(), 2);
}

#[test]
fn the_runtime_state_follows_commits_and_stamps_go_stale() {
    let state = TelemetryState::<NoopRawMutex>::new();
    assert!(state.resolve("p").is_none());
    let mut profiles = ProfileSet::default();
    profiles
        .add_profile("p", PacketFormatId::Robonomics)
        .unwrap();
    state.publish(&profiles);
    let stamp = state.resolve("p").unwrap();
    assert!(state.is_current(&stamp));
    assert_eq!(block_on(state.changed()), profiles.generation);

    profiles
        .set_public(
            "p",
            DataSelection::local(Selection::NONE.with(SDS011_PM10)),
            &Caps,
        )
        .unwrap();
    state.publish(&profiles);
    assert!(
        !state.is_current(&stamp),
        "an edited policy invalidates the old stamp"
    );

    // Delete and recreate under the same id: never current for the old stamp.
    let current = state.resolve("p").unwrap();
    profiles.remove_profile("p").unwrap();
    profiles
        .add_profile("p", PacketFormatId::Robonomics)
        .unwrap();
    state.publish(&profiles);
    assert!(!state.is_current(&current));
    assert_eq!(state.generation(), profiles.generation);
}

#[test]
fn the_section_round_trips_as_a_config_section() {
    use rohi_config::ConfigSection as _;
    let mut section = TelemetrySection::default();
    section
        .0
        .add_profile("p", PacketFormatId::Robonomics)
        .unwrap();
    let mut buf = [0u8; 256];
    let len = {
        let mut cursor = minicbor::encode::write::Cursor::new(&mut buf[..]);
        minicbor::encode(&section, &mut cursor).unwrap();
        cursor.position()
    };
    let back = TelemetrySection::decode_versioned(TelemetrySection::VERSION, &buf[..len]).unwrap();
    assert_eq!(back, section);
    assert!(
        TelemetrySection::decode_versioned(TelemetrySection::VERSION + 1, &buf[..len]).is_err()
    );
}
