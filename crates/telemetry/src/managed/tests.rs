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
use core::cell::RefCell;

use std::vec;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_service::{Action, ActionError, Service, Status};
use rohi_service::{ActionRegistry, MAX_FIELD_VALUE_LEN, MAX_FIELDS};

use super::*;
use crate::catalog::{BMX280_TEMPERATURE, LOCATION, SDS011_PM10, all_cataloged};

// A device keeps two slots; three exercise the largest supported schema.
type ProfileSet = TelemetryConfig<2>;

struct Caps;

impl FormatCapabilities for Caps {
    fn supported(&self) -> Selection {
        // The format cannot encode the last measurement.
        all_cataloged().without(crate::MeasurementId(17))
    }

    fn accepts_recipient(&self, recipient: &RecipientAddress) -> bool {
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

impl TelemetryStore<2> for &Memory {
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

type Service_<'a> = TelemetryService<NoopRawMutex, &'a Memory, Caps, 2>;

fn service(memory: &Memory) -> Service_<'_> {
    TelemetryService::new(memory, Caps)
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

fn set_public(
    service: &Service_<'_>,
    index: u64,
    selection: &Selection,
) -> Result<(), ActionError> {
    call(
        service,
        action::SET_PUBLIC,
        &[(1, Value::U64(index)), (2, bytes(selection.as_bytes()))],
    )
    .map(|_| ())
}

fn set_private(
    service: &Service_<'_>,
    index: u64,
    key: u8,
    selection: &Selection,
) -> Result<(), ActionError> {
    call(
        service,
        action::SET_PRIVATE,
        &[
            (1, Value::U64(index)),
            (2, bytes(&[key; 32])),
            (3, bytes(selection.as_bytes())),
        ],
    )
    .map(|_| ())
}

#[test]
fn the_service_exposes_exactly_the_three_actions_with_their_access() {
    let actions: std::vec::Vec<_> = Service_::ACTIONS
        .iter()
        .map(|a| {
            (
                a.name,
                a.access,
                a.args
                    .iter()
                    .map(|arg| arg.name)
                    .collect::<std::vec::Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        actions,
        [
            (
                "set_public",
                ActionAccess::Mutate,
                vec!["index", "selection"]
            ),
            (
                "set_private",
                ActionAccess::Mutate,
                vec!["index", "recipient", "selection"]
            ),
            ("measurements", ActionAccess::Read, vec!["offset"]),
        ]
    );
    // The selection fields point at the catalog action of this service.
    for (action, field) in [(0, 1), (1, 2)] {
        let selection = Service_::ACTIONS[action].args[field].selection.unwrap();
        assert_eq!(
            (
                selection.size_bytes,
                selection.catalog_service_id,
                selection.catalog_action_id
            ),
            (32, Service_::ID, action::MEASUREMENTS)
        );
    }
    assert_eq!(Service_::ACTIONS[1].args[1].kind, ValueKind::Address);
    assert_eq!(Service_::NAME, "telemetry");
}

#[test]
fn both_slots_start_empty() {
    let memory = Memory::default();
    let _service = service(&memory);
    for index in 0..2 {
        let profile = memory.stored.borrow().profiles[index];
        assert!(profile.public.measurements.is_none() && profile.private.is_none());
    }
    assert_eq!(*memory.commits.borrow(), 0);
}

#[test]
fn set_public_replaces_and_unsupported_bits_are_rejected_without_a_commit() {
    let memory = Memory::default();
    let service = service(&memory);
    set_public(&service, 0, &Selection::NONE.with(BMX280_TEMPERATURE)).unwrap();
    set_public(&service, 0, &Selection::NONE.with(SDS011_PM10)).unwrap();
    let public = memory.stored.borrow().profiles[0].public.measurements;
    assert!(public.contains(SDS011_PM10) && !public.contains(BMX280_TEMPERATURE));
    assert!(
        memory.stored.borrow().profiles[1]
            .public
            .measurements
            .is_none()
    );

    let commits = *memory.commits.borrow();
    // 17 is cataloged but the format cannot encode it; 40 is unknown.
    for bit in [17, 40] {
        assert_eq!(
            set_public(
                &service,
                0,
                &Selection::NONE.with(crate::MeasurementId(bit))
            ),
            Err(ActionError::Rejected("unsupported_measurement"))
        );
    }
    assert_eq!(*memory.commits.borrow(), commits);
}

#[test]
fn indices_zero_and_one_work_and_two_is_rejected_without_a_commit() {
    let memory = Memory::default();
    let service = service(&memory);
    set_public(&service, 0, &Selection::ALL).unwrap();
    set_public(&service, 1, &Selection::ALL).unwrap();
    set_private(&service, 1, 1, &Selection::NONE.with(LOCATION)).unwrap();
    let commits = *memory.commits.borrow();
    for index in [2, 3, 255, 256, u64::MAX] {
        assert_eq!(
            set_public(&service, index, &Selection::ALL),
            Err(ActionError::Rejected("invalid_index")),
            "{index}"
        );
        assert_eq!(
            set_private(&service, index, 1, &Selection::NONE.with(LOCATION)),
            Err(ActionError::Rejected("invalid_index")),
            "{index}"
        );
    }
    assert_eq!(*memory.commits.borrow(), commits);
    // The wrong type of the index is a malformed argument.
    assert_eq!(
        call(
            &service,
            action::SET_PUBLIC,
            &[
                (1, Value::Bool(true)),
                (2, bytes(Selection::ALL.as_bytes()))
            ]
        ),
        Err(ActionError::InvalidArgument)
    );
}

#[test]
fn set_private_replaces_the_recipient_and_none_removes_the_section() {
    let memory = Memory::default();
    let service = service(&memory);
    set_private(&service, 0, 1, &Selection::NONE.with(LOCATION)).unwrap();
    set_private(&service, 0, 2, &Selection::NONE.with(SDS011_PM10)).unwrap();
    let section = memory.stored.borrow().profiles[0].private.unwrap();
    assert_eq!(section.recipient, RecipientAddress([2; 32]));
    assert!(section.selection.measurements.contains(SDS011_PM10));
    assert!(memory.stored.borrow().profiles[1].private.is_none());

    assert_eq!(
        set_private(&service, 0, 0, &Selection::ALL),
        Err(ActionError::Rejected("invalid_recipient"))
    );
    assert_eq!(
        set_private(
            &service,
            0,
            3,
            &Selection::NONE.with(crate::MeasurementId(17))
        ),
        Err(ActionError::Rejected("unsupported_measurement"))
    );
    // The private section is untouched by the refusals.
    assert_eq!(memory.stored.borrow().profiles[0].private.unwrap(), section);

    set_private(&service, 0, 2, &Selection::NONE).unwrap();
    assert!(memory.stored.borrow().profiles[0].private.is_none());
    // Removing what is not there is not an error.
    set_private(&service, 0, 2, &Selection::NONE).unwrap();
}

#[test]
fn a_failed_commit_changes_nothing() {
    let memory = Memory::default();
    let service = service(&memory);
    set_public(&service, 0, &Selection::NONE.with(BMX280_TEMPERATURE)).unwrap();
    let before = memory.stored.borrow().clone();
    *memory.fail_commit.borrow_mut() = true;
    assert_eq!(
        set_public(&service, 0, &Selection::NONE.with(SDS011_PM10)),
        Err(ActionError::Internal)
    );
    assert_eq!(*memory.stored.borrow(), before);
}

#[test]
fn an_unreadable_store_is_an_error_not_an_empty_policy() {
    let memory = Memory::default();
    let service = service(&memory);
    *memory.unreadable.borrow_mut() = true;
    assert_eq!(
        set_public(&service, 0, &Selection::ALL),
        Err(ActionError::Unavailable)
    );
    let summary = block_on(service.summary());
    assert_eq!(summary.health, ServiceHealth::Error);
}

#[test]
fn arguments_of_the_wrong_shape_are_invalid() {
    let memory = Memory::default();
    let service = service(&memory);
    // A selection is exactly 32 bytes; the registry checks it before the action.
    assert_eq!(
        call(
            &service,
            action::SET_PUBLIC,
            &[(1, Value::U64(0)), (2, bytes(&[0xFF; 31]))]
        ),
        Err(ActionError::InvalidArgument)
    );
    // So is an address, and no argument may be missing.
    assert_eq!(
        call(
            &service,
            action::SET_PRIVATE,
            &[
                (1, Value::U64(0)),
                (2, bytes(&[1; 31])),
                (3, bytes(Selection::ALL.as_bytes()))
            ]
        ),
        Err(ActionError::InvalidArgument)
    );
    assert_eq!(
        call(&service, action::SET_PUBLIC, &[(1, Value::U64(0))]),
        Err(ActionError::InvalidArgument)
    );
    // The removed Actions are unknown.
    for old in [4, 5, 6, 7, 8] {
        assert!(call(&service, old, &[]).is_err());
    }
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
    for index in 0..2 {
        set_public(&service, index, &Selection::ALL).unwrap();
        set_private(&service, index, 1 + index as u8, &Selection::ALL).unwrap();
    }
    assert!(minicbor::len(TelemetrySectionRef(&memory.stored.borrow())) <= MAX_SECTION_PAYLOAD);
}

struct Collect(std::vec::Vec<(u32, Value)>);

impl StatusWriter for Collect {
    fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
        self.0.push((id, value));
        Ok(())
    }
}

fn status(service: &Service_<'_>) -> std::vec::Vec<(u32, Value)> {
    let mut out = Collect(std::vec::Vec::new());
    block_on(service.status(&mut out)).unwrap();
    out.0
}

fn field(fields: &[(u32, Value)], id: u32) -> Option<&Value> {
    fields.iter().find(|(f, _)| *f == id).map(|(_, v)| v)
}

#[test]
fn the_status_schema_is_flat_unique_and_within_the_limits() {
    fn check<const N: usize>()
    where
        Slots<N>: ProfileSlots,
    {
        let schema = <Slots<N> as ProfileSlots>::SCHEMA;
        assert_eq!(schema.len(), 1 + 5 * N);
        assert!(schema.len() <= MAX_FIELDS);
        for (i, a) in schema.iter().enumerate() {
            assert!(!schema[..i].iter().any(|b| b.id == a.id || b.name == a.name));
            // Names are wire ABI: `[a-z][a-z0-9_]*` and at most 24 bytes.
            assert!(a.name.len() <= rohi_service::MAX_NAME_LEN, "{}", a.name);
            assert!(a.name.starts_with(|c: char| c.is_ascii_lowercase()));
            assert!(
                a.name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            );
        }
    }
    check::<1>();
    check::<2>();
    check::<3>();
    let names: std::vec::Vec<_> = <Service_ as Status>::STATUS_SCHEMA
        .iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(
        names,
        [
            "generation",
            "profile0_public",
            "profile0_private",
            "profile0_recipient",
            "profile0_priv_selection",
            "profile0_revision",
            "profile1_public",
            "profile1_private",
            "profile1_recipient",
            "profile1_priv_selection",
            "profile1_revision",
        ]
    );
}

#[test]
fn status_reports_each_slot_and_the_recipient_only_when_there_is_one() {
    let memory = Memory::default();
    let service = service(&memory);
    let schema = <Service_ as Status>::STATUS_SCHEMA;

    let fresh = status(&service);
    for (id, value) in &fresh {
        let descriptor = schema.iter().find(|f| f.id == *id).expect("in the schema");
        assert!(descriptor.admits(value), "{}", descriptor.name);
    }
    assert_eq!(
        field(&fresh, status_field::profile(0, status_field::PRIVATE)),
        Some(&Value::Bool(false))
    );
    assert!(field(&fresh, status_field::profile(0, status_field::RECIPIENT)).is_none());
    assert_eq!(
        field(&fresh, status_field::profile(1, status_field::REVISION)),
        Some(&Value::U64(0))
    );

    set_public(&service, 1, &Selection::NONE.with(SDS011_PM10)).unwrap();
    set_private(&service, 1, 7, &Selection::NONE.with(LOCATION)).unwrap();
    let fields = status(&service);
    for (id, value) in &fields {
        let descriptor = schema.iter().find(|f| f.id == *id).expect("in the schema");
        assert!(descriptor.admits(value), "{}", descriptor.name);
        if let Value::Bytes(b) = value {
            assert!(b.len() <= MAX_FIELD_VALUE_LEN);
        }
    }
    assert_eq!(
        field(&fields, status_field::profile(1, status_field::PRIVATE)),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        field(&fields, status_field::profile(1, status_field::RECIPIENT)).and_then(Value::as_bytes),
        Some(&[7u8; 32][..])
    );
    let public = field(&fields, status_field::profile(1, status_field::PUBLIC))
        .and_then(Value::as_bytes)
        .and_then(Selection::from_slice)
        .unwrap();
    assert!(public.contains(SDS011_PM10));
    assert_eq!(
        field(&fields, status_field::profile(1, status_field::REVISION)),
        Some(&Value::U64(2))
    );
    // Slot 0 is untouched.
    assert_eq!(
        field(&fields, status_field::profile(0, status_field::REVISION)),
        Some(&Value::U64(0))
    );
    assert!(field(&fields, status_field::profile(0, status_field::RECIPIENT)).is_none());
    assert_eq!(
        field(&fields, status_field::GENERATION),
        Some(&Value::U64(2))
    );
}

#[test]
fn the_runtime_state_follows_commits_and_stamps_go_stale() {
    let state = TelemetryState::<NoopRawMutex, 2>::new();
    assert!(state.resolve(2).is_none(), "no slot 2");
    let mut profiles = ProfileSet::default();
    state.publish(&profiles);
    let zero = state.resolve(0).unwrap();
    let one = state.resolve(1).unwrap();
    assert!(state.is_current(&zero) && state.is_current(&one));
    assert_eq!(block_on(state.changed()), profiles.generation);

    profiles
        .set_public(
            0,
            DataSelection::local(Selection::NONE.with(SDS011_PM10)),
            &Caps,
        )
        .unwrap();
    state.publish(&profiles);
    assert!(
        !state.is_current(&zero),
        "an edited policy invalidates the old stamp"
    );
    assert!(state.is_current(&one), "the other slot is untouched");
    assert!(!state.is_current(&ProfileStamp {
        profile_index: 5,
        revision: 0
    }));

    // A reset gives every slot a revision no earlier packet carries.
    let before = [state.resolve(0).unwrap(), one];
    profiles.clear();
    state.publish(&profiles);
    assert!(before.iter().all(|stamp| !state.is_current(stamp)));
    assert_eq!(state.generation(), profiles.generation);
    assert!(state.with_profile(2, |_| ()).is_none());
    assert!(
        state
            .with_profile(0, |profile| profile.public.measurements.is_none())
            .unwrap()
    );
}

#[test]
fn the_section_round_trips_as_a_config_section() {
    use rohi_config::ConfigSection as _;
    type Section = TelemetrySection<2>;
    let mut section = Section::default();
    section
        .0
        .set_public(0, DataSelection::local(Selection::ALL), &Caps)
        .unwrap();
    let mut buf = [0u8; 256];
    let len = rohi_config::encode_to_slice(&section, &mut buf).unwrap();
    let back = Section::decode_versioned(Section::VERSION, &buf[..len]).unwrap();
    assert_eq!(back, section);
    assert!(Section::decode_versioned(Section::VERSION + 1, &buf[..len]).is_err());
    // The named-profile section of version 1 is not read.
    assert!(Section::decode_versioned(1, &buf[..len]).is_err());
    assert_eq!(Section::VERSION, 2);
}

#[test]
fn a_section_of_another_capacity_is_refused_not_truncated() {
    type Big = TelemetrySection<3>;
    type Device = TelemetrySection<2>;
    let big = Big::default();
    let mut buf = [0u8; 512];
    let len = rohi_config::encode_to_slice(&big, &mut buf).unwrap();
    assert!(minicbor::decode::<Device>(&buf[..len]).is_err());

    // The identity of the section does not depend on the capacity.
    use rohi_config::ConfigSection as _;
    assert_eq!(Big::ID, Device::ID);
    assert_eq!(Device::ID.0, 0x7c1e_89f0_a500_7b17);
}
