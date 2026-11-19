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
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_service::{ActionRegistry, ActionResult, MAX_FIELDS};
use rohi_telemetry::ProfileStamp;

use super::*;
use crate::endpoint::MAX_ENDPOINT_LEN;
use crate::provider::BuildFailure;

// The capacities of the device: two route slots.
type RouteSet = crate::Routes<2>;

#[derive(Default)]
struct Memory {
    stored: RefCell<RouteSet>,
    fail_commit: RefCell<bool>,
    unreadable: RefCell<bool>,
    commits: RefCell<u32>,
}

impl ConnectivityStore<2> for &Memory {
    async fn load(&self) -> Result<RouteSet, StoreError> {
        if *self.unreadable.borrow() {
            return Err(StoreError::Unavailable);
        }
        Ok(self.stored.borrow().clone())
    }

    async fn commit(&self, candidate: &RouteSet) -> Result<(), StoreError> {
        if *self.fail_commit.borrow() {
            return Err(StoreError::Internal);
        }
        *self.stored.borrow_mut() = candidate.clone();
        *self.commits.borrow_mut() += 1;
        Ok(())
    }
}

struct Exporter<'a> {
    revision: &'a RefCell<u32>,
    fail: &'a RefCell<Option<BuildFailure>>,
    builds: &'a RefCell<u32>,
    now: &'a RefCell<u64>,
}

impl PacketExporter for &Exporter<'_> {
    fn now_ms(&self) -> u64 {
        *self.now.borrow()
    }

    // The device has two profile slots.
    fn resolve(&self, profile_index: u8) -> Option<ProfileStamp> {
        (profile_index < 2).then(|| ProfileStamp {
            profile_index,
            revision: *self.revision.borrow(),
        })
    }

    async fn export(
        &self,
        profile_index: u8,
        out: &mut [u8],
    ) -> Result<(ProfileStamp, usize), BuildFailure> {
        if let Some(failure) = *self.fail.borrow() {
            return Err(failure);
        }
        let stamp = self
            .resolve(profile_index)
            .ok_or(BuildFailure::ProfileChanged)?;
        *self.builds.borrow_mut() += 1;
        // A packet of 1200 bytes would not fit; 700 take two chunks.
        let nonce = *self.builds.borrow();
        for (index, byte) in out[..700].iter_mut().enumerate() {
            *byte = (index as u32 + nonce) as u8;
        }
        Ok((stamp, 700))
    }
}

struct Fixture {
    memory: Memory,
    revision: RefCell<u32>,
    fail: RefCell<Option<BuildFailure>>,
    builds: RefCell<u32>,
    now: RefCell<u64>,
    state: ConnectivityState<NoopRawMutex, 2>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            memory: Memory::default(),
            revision: RefCell::new(1),
            fail: RefCell::new(None),
            builds: RefCell::new(0),
            now: RefCell::new(0),
            state: ConnectivityState::new(),
        }
    }

    fn exporter(&self) -> Exporter<'_> {
        Exporter {
            revision: &self.revision,
            fail: &self.fail,
            builds: &self.builds,
            now: &self.now,
        }
    }
}

type Svc<'a, 'e> = ConnectivityService<'a, NoopRawMutex, &'a Memory, &'e Exporter<'e>, 2, 2, 1024>;

fn text(value: &str) -> Value {
    Value::text(value).unwrap()
}

fn call(
    service: &Svc<'_, '_>,
    action: u32,
    args: &[(u32, Value)],
) -> Result<ActionResult, ActionError> {
    let registry = (service,);
    block_on(registry.invoke(Svc::ID, action, args))
}

fn set_endpoint(
    service: &Svc<'_, '_>,
    index: u64,
    profile: u64,
    endpoint: &str,
) -> Result<(), ActionError> {
    call(
        service,
        action::SET_ENDPOINT,
        &[
            (action_field::INDEX, Value::U64(index)),
            (action_field::PROFILE_INDEX, Value::U64(profile)),
            (action_field::ENDPOINT, text(endpoint)),
        ],
    )
    .map(|_| ())
}

fn set_interval(service: &Svc<'_, '_>, index: u64, seconds: u64) -> Result<(), ActionError> {
    call(
        service,
        action::SET_INTERVAL,
        &[
            (action_field::INDEX, Value::U64(index)),
            (action_field::INTERVAL_S, Value::U64(seconds)),
        ],
    )
    .map(|_| ())
}

fn set_fallback(service: &Svc<'_, '_>, index: u64, endpoint: &str) -> Result<(), ActionError> {
    call(
        service,
        action::SET_FALLBACK,
        &[
            (action_field::INDEX, Value::U64(index)),
            (action_field::FALLBACK_ENDPOINT, text(endpoint)),
        ],
    )
    .map(|_| ())
}

fn rejected(code: &'static str) -> ActionError {
    ActionError::Rejected(code)
}

#[test]
fn the_actions_are_the_four_documented_ones() {
    let actions: Vec<_> = Svc::ACTIONS
        .iter()
        .map(|a| (a.name, a.access, a.args.len(), a.stream))
        .collect();
    assert_eq!(
        actions,
        [
            ("set_endpoint", ActionAccess::Mutate, 3, false),
            ("set_interval", ActionAccess::Mutate, 2, false),
            ("set_fallback", ActionAccess::Mutate, 2, false),
            #[cfg(feature = "packet-export")]
            ("get_payload", ActionAccess::Read, 1, true),
        ]
    );
    let args = |name: &str| -> Vec<&str> {
        Svc::ACTIONS
            .iter()
            .find(|a| a.name == name)
            .unwrap()
            .args
            .iter()
            .map(|f| f.name)
            .collect()
    };
    assert_eq!(args("set_endpoint"), ["index", "profile_index", "endpoint"]);
    assert_eq!(args("set_interval"), ["index", "interval_s"]);
    assert_eq!(args("set_fallback"), ["index", "endpoint"]);
    assert_eq!(Svc::NAME, "connectivity");
}

#[test]
fn a_route_needs_an_existing_profile_slot_and_starts_with_the_defaults() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    for profile in [2, 3, 255] {
        assert_eq!(
            set_endpoint(&service, 0, profile, "http://a.example").unwrap_err(),
            rejected("invalid_profile_index")
        );
    }
    assert_eq!(
        set_endpoint(&service, 0, 256, "http://a.example").unwrap_err(),
        rejected("invalid_profile_index")
    );
    assert_eq!(fixture.memory.stored.borrow().count(), 0);

    set_endpoint(&service, 0, 1, "http://a.example").unwrap();
    let stored = fixture.memory.stored.borrow();
    let route = stored.get(0).unwrap();
    assert_eq!(route.profile_index, 1);
    assert_eq!(route.interval_s, crate::DEFAULT_INTERVAL_S);
    assert_eq!(route.fallback, None);
    assert!(stored.get(1).is_none());
}

#[test]
fn route_indices_0_and_1_are_slots_and_2_and_beyond_are_rejected() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    set_endpoint(&service, 0, 0, "http://a.example").unwrap();
    set_endpoint(&service, 1, 0, "http://b.example").unwrap();
    let before = fixture.memory.stored.borrow().clone();
    for index in [2, 3, u64::MAX] {
        for result in [
            set_endpoint(&service, index, 0, "http://c.example"),
            set_endpoint(&service, index, 0, ""),
            set_interval(&service, index, 60),
            set_fallback(&service, index, "http://f.example"),
            set_fallback(&service, index, ""),
        ] {
            assert_eq!(result.unwrap_err(), rejected("invalid_index"), "{index}");
        }
    }
    assert_eq!(*fixture.memory.stored.borrow(), before);
}

#[test]
fn an_empty_endpoint_is_absent_and_deletes_the_route_or_the_fallback() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    set_endpoint(&service, 0, 0, "http://a.example").unwrap();
    set_endpoint(&service, 1, 1, "http://b.example").unwrap();
    set_fallback(&service, 0, "http://f.example").unwrap();
    assert!(
        fixture
            .memory
            .stored
            .borrow()
            .get(0)
            .unwrap()
            .fallback
            .is_some()
    );

    set_fallback(&service, 0, "").unwrap();
    assert_eq!(
        fixture.memory.stored.borrow().get(0).unwrap().fallback,
        None
    );

    // Deleting the route of slot 0 leaves slot 1 exactly where it is.
    set_endpoint(&service, 0, 0, "").unwrap();
    assert!(fixture.memory.stored.borrow().get(0).is_none());
    assert_eq!(
        fixture
            .memory
            .stored
            .borrow()
            .get(1)
            .unwrap()
            .endpoint
            .as_str(),
        "http://b.example"
    );
    // An empty slot can be "deleted" again, even for a profile that does not exist.
    set_endpoint(&service, 0, 9, "").unwrap();
    // Its other edits need a route.
    assert_eq!(
        set_interval(&service, 0, 60).unwrap_err(),
        rejected("route_not_found")
    );
    assert_eq!(
        set_fallback(&service, 0, "http://f.example").unwrap_err(),
        rejected("route_not_found")
    );
    assert_eq!(
        set_fallback(&service, 0, "").unwrap_err(),
        rejected("route_not_found")
    );
}

#[test]
fn a_missing_endpoint_argument_is_refused_by_the_registry_but_means_absent_to_the_service() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    set_endpoint(&service, 0, 0, "http://a.example").unwrap();
    // The registry requires every declared argument: the wire form of "absent" is "".
    assert_eq!(
        call(
            &service,
            action::SET_ENDPOINT,
            &[
                (action_field::INDEX, Value::U64(0)),
                (action_field::PROFILE_INDEX, Value::U64(0)),
            ],
        )
        .unwrap_err(),
        ActionError::InvalidArgument
    );
    assert!(fixture.memory.stored.borrow().get(0).is_some());
    // Called directly (an optional argument), the absent argument deletes.
    block_on(service.invoke(
        action::SET_ENDPOINT,
        &[
            (action_field::INDEX, Value::U64(0)),
            (action_field::PROFILE_INDEX, Value::U64(0)),
        ],
        &mut NoOutput,
    ))
    .unwrap();
    assert!(fixture.memory.stored.borrow().get(0).is_none());
}

struct NoOutput;

impl StatusWriter for NoOutput {
    fn put(&mut self, _: u32, _: Value) -> Result<(), WriteError> {
        Ok(())
    }
}

impl ActionOutput for NoOutput {
    fn next_record(&mut self) -> Result<(), WriteError> {
        Ok(())
    }
}

#[test]
fn arguments_are_validated_and_a_refused_change_commits_nothing() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    for bad in [
        "https://a.example",
        "http://a.example/x",
        "a.example",
        "http://",
        "x",
    ] {
        assert_eq!(
            set_endpoint(&service, 0, 0, bad).unwrap_err(),
            rejected("invalid_endpoint"),
            "{bad:?}"
        );
    }
    // A text argument cannot exceed MAX_FIELD_VALUE_LEN, which is the endpoint limit.
    assert!(Value::text(&"h".repeat(MAX_ENDPOINT_LEN + 1)).is_none());
    assert_eq!(*fixture.memory.commits.borrow(), 0);

    // Wrong kinds of argument.
    assert_eq!(
        call(
            &service,
            action::SET_INTERVAL,
            &[(1, text("0")), (2, Value::U64(60))]
        )
        .unwrap_err(),
        ActionError::InvalidArgument
    );

    set_endpoint(&service, 0, 0, "http://a.example").unwrap();
    for bad in [0u64, 9, 86_401, u64::MAX] {
        assert_eq!(
            set_interval(&service, 0, bad).unwrap_err(),
            rejected("invalid_interval")
        );
    }
    set_interval(&service, 0, 120).unwrap();
    assert_eq!(
        fixture.memory.stored.borrow().get(0).unwrap().interval_s,
        120
    );
    assert_eq!(
        set_fallback(&service, 0, "http://a.example").unwrap_err(),
        rejected("duplicate_endpoint"),
        "the fallback must differ from the primary"
    );
    assert_eq!(
        set_fallback(&service, 0, "https://f.example").unwrap_err(),
        rejected("invalid_endpoint")
    );
    assert_eq!(*fixture.memory.commits.borrow(), 2);
}

#[test]
fn replacing_a_route_keeps_the_interval_and_fallback_and_a_recreated_one_starts_clean() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    set_endpoint(&service, 1, 0, "http://a.example").unwrap();
    set_interval(&service, 1, 300).unwrap();
    set_fallback(&service, 1, "http://f.example").unwrap();
    let first = fixture.memory.stored.borrow().get(1).unwrap().generation;

    set_endpoint(&service, 1, 1, "http://b.example").unwrap();
    {
        let stored = fixture.memory.stored.borrow();
        let route = stored.get(1).unwrap();
        assert_eq!((route.profile_index, route.interval_s), (1, 300));
        assert_eq!(
            route.fallback.as_ref().unwrap().as_str(),
            "http://f.example"
        );
        assert!(route.generation > first);
    }

    set_endpoint(&service, 1, 1, "").unwrap();
    set_endpoint(&service, 1, 1, "http://b.example").unwrap();
    let stored = fixture.memory.stored.borrow();
    let route = stored.get(1).unwrap();
    assert_eq!(route.interval_s, 60);
    assert_eq!(route.fallback, None);
}

#[test]
fn a_failed_commit_or_an_unreadable_store_changes_nothing() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    set_endpoint(&service, 0, 0, "http://a.example").unwrap();
    let before = fixture.memory.stored.borrow().clone();
    *fixture.memory.fail_commit.borrow_mut() = true;
    assert_eq!(
        set_endpoint(&service, 1, 0, "http://b.example").unwrap_err(),
        ActionError::Internal
    );
    assert_eq!(*fixture.memory.stored.borrow(), before);
    *fixture.memory.fail_commit.borrow_mut() = false;
    *fixture.memory.unreadable.borrow_mut() = true;
    assert_eq!(
        set_endpoint(&service, 1, 0, "http://b.example").unwrap_err(),
        ActionError::Unavailable
    );
}

struct Collect(Vec<(u32, Value)>);

impl StatusWriter for Collect {
    fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
        self.0.push((id, value));
        Ok(())
    }
}

fn status(service: &Svc<'_, '_>) -> Vec<(u32, Value)> {
    let mut out = Collect(Vec::new());
    block_on(service.status(&mut out)).unwrap();
    out.0
}

fn field(fields: &[(u32, Value)], id: u32) -> Option<&Value> {
    fields.iter().find(|(f, _)| *f == id).map(|(_, v)| v)
}

fn view(state: RouteState, queue_depth: usize, generation: u32) -> crate::router::RouteView {
    crate::router::RouteView {
        generation,
        state,
        queue_depth,
        counters: crate::router::RouteCounters {
            delivered: 5,
            retries: 2,
            ..Default::default()
        },
        active_path: Some(0),
    }
}

#[test]
fn status_summary_follows_the_routes() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    assert_eq!(
        block_on(service.summary()).state,
        ServiceState::Stopped,
        "no route is a valid state"
    );
    fixture.state.mark_unreadable();
    assert_eq!(block_on(service.summary()).state, ServiceState::Failed);
}

#[test]
fn status_reports_empty_slots_apart_from_active_ones_and_never_leaks_a_stale_view() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);

    // Both slots empty: only the state of each.
    let empty = status(&service);
    assert_eq!(
        field(&empty, status_field::ACTIVE_ROUTES),
        Some(&Value::U64(0))
    );
    for slot in 0..2 {
        assert_eq!(
            field(&empty, status_field::route(slot, status_field::STATE)),
            Some(&Value::Enum(0)),
            "empty"
        );
        assert!(field(&empty, status_field::route(slot, status_field::ENDPOINT)).is_none());
    }

    let mut routes = RouteSet::default();
    routes.set_endpoint(1, 1, Some("http://a.example")).unwrap();
    routes.set_fallback(1, Some("http://f.example")).unwrap();
    routes.set_interval(1, 90).unwrap();
    fixture.state.publish(&routes);
    let generation = routes.get(1).unwrap().generation;
    let mut views = [None; 2];
    views[1] = Some(view(RouteState::Delivering, 2, generation));
    fixture.state.report(views, crate::state::Health::default());

    let fields = status(&service);
    let id = |n| status_field::route(1, n);
    assert_eq!(
        field(&fields, status_field::route(0, status_field::STATE)),
        Some(&Value::Enum(0))
    );
    assert_eq!(
        field(&fields, id(status_field::STATE)),
        Some(&Value::Enum(2))
    );
    assert_eq!(
        field(&fields, id(status_field::PROFILE)),
        Some(&Value::U64(1))
    );
    assert_eq!(
        field(&fields, id(status_field::ENDPOINT)).unwrap().as_str(),
        Some("http://a.example")
    );
    assert_eq!(
        field(&fields, id(status_field::FALLBACK)).unwrap().as_str(),
        Some("http://f.example")
    );
    assert_eq!(
        field(&fields, id(status_field::INTERVAL_S)),
        Some(&Value::U64(90))
    );
    assert_eq!(
        field(&fields, id(status_field::QUEUE_DEPTH)),
        Some(&Value::U64(2))
    );
    assert_eq!(
        field(&fields, id(status_field::DELIVERED)),
        Some(&Value::U64(5))
    );
    assert_eq!(
        field(&fields, id(status_field::ACTIVE_PATH)),
        Some(&Value::U64(1))
    );
    assert_eq!(
        field(&fields, status_field::TOTAL_QUEUE_DEPTH),
        Some(&Value::U64(2))
    );
    assert_eq!(
        field(&fields, status_field::ACTIVE_ROUTES),
        Some(&Value::U64(1))
    );

    // The slot is deleted and reused before the publisher looked again: the old
    // view is not this route's, so it reports a clean one.
    routes.set_endpoint(1, 0, None).unwrap();
    routes.set_endpoint(1, 0, Some("http://c.example")).unwrap();
    fixture.state.publish(&routes);
    let fields = status(&service);
    assert_eq!(
        field(&fields, id(status_field::STATE)),
        Some(&Value::Enum(1))
    );
    assert_eq!(
        field(&fields, id(status_field::QUEUE_DEPTH)),
        Some(&Value::U64(0))
    );
    assert_eq!(
        field(&fields, id(status_field::DELIVERED)),
        Some(&Value::U64(0))
    );
    assert_eq!(
        field(&fields, id(status_field::RETRIES)),
        Some(&Value::U64(0))
    );
    assert!(field(&fields, id(status_field::FALLBACK)).is_none());
    assert_eq!(
        field(&fields, status_field::TOTAL_QUEUE_DEPTH),
        Some(&Value::U64(0))
    );
}

#[test]
fn the_longest_endpoints_fit_the_status_and_the_section() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    let endpoint = std::format!("http://{}.example.org:8080", "h".repeat(40));
    let fallback = std::format!("http://{}.example.org:8081", "g".repeat(40));
    assert_eq!(
        (endpoint.len(), fallback.len()),
        (MAX_ENDPOINT_LEN, MAX_ENDPOINT_LEN)
    );
    for index in 0..2 {
        set_endpoint(&service, index, 1, &endpoint).unwrap();
        set_fallback(&service, index, &fallback).unwrap();
    }
    fixture.state.publish(&fixture.memory.stored.borrow());
    let fields = status(&service);
    assert_eq!(
        field(&fields, status_field::route(1, status_field::ENDPOINT))
            .unwrap()
            .as_str(),
        Some(endpoint.as_str()),
        "not truncated"
    );
    assert!(fields.len() <= MAX_FIELDS);
    assert!(minicbor::len(SectionRef(&fixture.memory.stored.borrow())) <= MAX_SECTION_PAYLOAD);
}

#[test]
fn failing_deliveries_degrade_the_health_even_when_packets_are_built() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    let mut routes = RouteSet::default();
    routes.set_endpoint(0, 0, Some("http://a.example")).unwrap();
    fixture.state.publish(&routes);
    let generation = routes.get(0).unwrap().generation;

    // Delivering normally: healthy.
    let report = |state| {
        let mut views = [None; 2];
        views[0] = Some(view(state, 1, generation));
        fixture.state.report(views, crate::state::Health::default());
    };
    report(RouteState::Delivering);
    assert_eq!(block_on(service.summary()).health, ServiceHealth::Ok);

    // Every path is failing and cooling down: degraded, with an issue.
    report(RouteState::Backoff);
    assert_eq!(block_on(service.summary()).health, ServiceHealth::Degraded);
    let mut issues = heapless::Vec::new();
    block_on(service.issues(&mut issues));
    assert!(issues.iter().any(|i| i.code == issue::DELIVERY_FAILING));
}

mod static_schema {
    use rohi_service::{FieldDescriptor, ValueKind};

    use crate::managed::{RouteSlots, Slots, status_field};

    fn names<const N: usize>() -> std::vec::Vec<&'static str>
    where
        Slots<N>: RouteSlots,
    {
        <Slots<N> as RouteSlots>::SCHEMA
            .iter()
            .map(|f| f.name)
            .collect()
    }

    #[test]
    fn the_schema_has_the_fields_of_the_selected_route_count_only() {
        assert_eq!(<Slots<1> as RouteSlots>::SCHEMA.len(), 2 + 9);
        assert_eq!(<Slots<2> as RouteSlots>::SCHEMA.len(), 2 + 2 * 9);
        assert!(<Slots<2> as RouteSlots>::SCHEMA.len() <= rohi_service::MAX_FIELDS);
        let two = names::<2>();
        assert_eq!(&two[..2], ["active_routes", "total_queue_depth"]);
        assert!(two.contains(&"route1_active_path"));
        assert!(two.contains(&"route0_endpoint"));
        assert!(!two.contains(&"route2_state"));
        // Names are wire ABI: `[a-z][a-z0-9_]*` and at most 24 bytes.
        for name in &two {
            assert!(name.len() <= rohi_service::MAX_NAME_LEN, "{name}");
            assert!(name.starts_with(|c: char| c.is_ascii_lowercase()), "{name}");
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{name}"
            );
        }
    }

    #[test]
    fn ids_names_and_kinds_are_stable_and_unique() {
        let schema = <Slots<2> as RouteSlots>::SCHEMA;
        let find =
            |name: &str| -> FieldDescriptor { *schema.iter().find(|f| f.name == name).unwrap() };
        assert_eq!(find("active_routes").id, status_field::ACTIVE_ROUTES);
        assert_eq!(find("route0_profile").id, 10);
        assert_eq!(find("route0_profile").kind, ValueKind::U64);
        assert_eq!(find("route0_endpoint").id, 11);
        assert_eq!(find("route0_endpoint").kind, ValueKind::String);
        assert_eq!(find("route0_fallback").id, 12);
        assert_eq!(find("route0_state").id, 14);
        assert_eq!(find("route0_state").kind, ValueKind::Enum);
        assert_eq!(find("route1_queue_depth").id, 25);
        assert_eq!(find("route1_active_path").id, 28);
        assert_eq!(find("route1_delivered").kind, ValueKind::U64);
        let mut ids: std::vec::Vec<u32> = schema.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), schema.len());
        let mut names = names::<2>();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), schema.len());
    }
}

#[cfg(feature = "packet-export")]
fn packet(service: &Svc<'_, '_>, profile: u64) -> Result<(u32, u64), ActionError> {
    match call(
        service,
        action::GET_PAYLOAD,
        &[(action_field::PAYLOAD_PROFILE_INDEX, Value::U64(profile))],
    )? {
        ActionResult::Stream(info) => {
            assert_eq!(info.files.len(), 1);
            assert_eq!(info.files[0].name.as_str(), "packet.bin");
            Ok((info.id, info.files[0].size_bytes))
        }
        ActionResult::Records(_) => panic!("get_payload returns a stream"),
    }
}

#[cfg(feature = "packet-export")]
fn read(service: &Svc<'_, '_>, stream: u32, offset: u64) -> Result<(Vec<u8>, bool), ActionError> {
    let mut buf = [0u8; 512];
    let registry = (service,);
    let chunk = block_on(registry.read_stream(Svc::ID, stream, 1, offset, &mut buf))?;
    Ok((buf[..chunk.len].to_vec(), chunk.eof))
}

#[cfg(feature = "packet-export")]
#[test]
fn get_payload_streams_one_immutable_packet_without_any_route() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    assert_eq!(fixture.memory.stored.borrow().count(), 0);
    let (stream, size) = packet(&service, 0).unwrap();
    assert_eq!(size, 700);
    assert_eq!(
        fixture.memory.stored.borrow().count(),
        0,
        "no route is created"
    );
    assert_eq!(*fixture.memory.commits.borrow(), 0);

    let (first, eof) = read(&service, stream, 0).unwrap();
    assert_eq!((first.len(), eof), (512, false));
    let (second, eof) = read(&service, stream, 512).unwrap();
    assert_eq!((second.len(), eof), (188, true));
    // Repeatable and idempotent: no new build, the same bytes.
    assert_eq!(read(&service, stream, 0).unwrap().0, first);
    assert_eq!(read(&service, stream, 512).unwrap().0, second);
    assert_eq!(*fixture.builds.borrow(), 1);
    assert_eq!(
        read(&service, stream, 701).unwrap_err(),
        ActionError::InvalidArgument
    );

    let registry = (&service,);
    block_on(registry.close_stream(Svc::ID, stream)).unwrap();
    assert_eq!(
        read(&service, stream, 0).unwrap_err(),
        ActionError::UnknownStream
    );
    assert_eq!(service.open_streams(), 0);
}

#[cfg(feature = "packet-export")]
#[test]
fn get_payload_takes_the_profile_slots_0_and_1_only() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    assert!(packet(&service, 0).is_ok());
    assert!(packet(&service, 1).is_ok());
    for index in [2, 3, 256, u64::MAX] {
        assert_eq!(
            packet(&service, index).unwrap_err(),
            rejected("invalid_profile_index"),
            "{index}"
        );
    }
}

#[cfg(feature = "packet-export")]
#[test]
fn concurrent_streams_are_bounded_and_never_overwrite_each_other() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    let (a, _) = packet(&service, 0).unwrap();
    let (b, _) = packet(&service, 0).unwrap();
    assert_ne!(a, b);
    assert_eq!(packet(&service, 0).unwrap_err(), ActionError::Busy);
    // Each stream keeps its own bytes.
    assert_ne!(
        read(&service, a, 0).unwrap().0,
        read(&service, b, 0).unwrap().0
    );
    // A closed stream makes room.
    let registry = (&service,);
    block_on(registry.close_stream(Svc::ID, a)).unwrap();
    assert!(packet(&service, 0).is_ok());
    // Losing the link releases everything.
    registry.link_lost();
    assert_eq!(service.open_streams(), 0);
}

#[cfg(feature = "packet-export")]
#[test]
fn a_stream_is_not_served_after_its_policy_changed_and_failures_are_stable_codes() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    let (stream, _) = packet(&service, 0).unwrap();
    *fixture.revision.borrow_mut() = 2;
    assert_eq!(
        read(&service, stream, 0).unwrap_err(),
        ActionError::UnknownStream
    );
    assert_eq!(service.open_streams(), 0, "the revoked packet was released");

    *fixture.fail.borrow_mut() = Some(BuildFailure::TimeUnavailable);
    assert_eq!(
        packet(&service, 0).unwrap_err(),
        rejected("time_unavailable")
    );
    *fixture.fail.borrow_mut() = Some(BuildFailure::NothingToPackage);
    assert_eq!(
        packet(&service, 0).unwrap_err(),
        rejected("nothing_to_package")
    );
    assert_eq!(service.open_streams(), 0);
}

#[cfg(feature = "packet-export")]
#[test]
fn an_abandoned_stream_expires_and_makes_room() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    let (a, _) = packet(&service, 0).unwrap();
    let (_b, _) = packet(&service, 0).unwrap();
    assert_eq!(packet(&service, 0).unwrap_err(), ActionError::Busy);

    *fixture.now.borrow_mut() = EXPORT_TTL_MS - 1;
    assert!(read(&service, a, 0).is_ok(), "still within its lifetime");
    *fixture.now.borrow_mut() = EXPORT_TTL_MS;
    assert_eq!(
        read(&service, a, 0).unwrap_err(),
        ActionError::UnknownStream
    );
    assert_eq!(service.open_streams(), 0);
    assert!(packet(&service, 0).is_ok());
}

#[cfg(feature = "packet-export")]
#[test]
fn an_expired_stream_does_not_block_a_new_get_payload() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    // One slot only: the abandoned stream holds all of the capacity.
    type One<'a> = ConnectivityService<'a, NoopRawMutex, &'a Memory, &'a Exporter<'a>, 2, 1, 1024>;
    let service: One<'_> = ConnectivityService::new(&fixture.memory, &exporter, &fixture.state);
    let registry = (&service,);
    let open = |service: &One<'_>| {
        block_on((service,).invoke(
            One::ID,
            action::GET_PAYLOAD,
            &[(action_field::PAYLOAD_PROFILE_INDEX, Value::U64(0))],
        ))
    };
    assert!(open(&service).is_ok());
    assert_eq!(open(&service).unwrap_err(), ActionError::Busy);

    // Past its lifetime, with no read of the old stream in between.
    *fixture.now.borrow_mut() = EXPORT_TTL_MS;
    assert!(open(&service).is_ok(), "the new request reclaims the slot");
    assert_eq!(service.open_streams(), 1);
    registry.link_lost();
}
