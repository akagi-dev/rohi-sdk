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
use std::borrow::ToOwned;
use std::string::String;
use std::vec;
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_service::{ActionRegistry, ActionResult};
use rohi_telemetry::{PacketProfileId, ProfileStamp};

use super::*;

// The capacities the tests exercise: more than a device keeps, to cover every edge.
type RouteSet = crate::Routes<3, 2>;
use crate::provider::BuildFailure;

#[derive(Default)]
struct Memory {
    stored: RefCell<RouteSet>,
    fail_commit: RefCell<bool>,
    unreadable: RefCell<bool>,
    commits: RefCell<u32>,
}

impl ConnectivityStore<3, 2> for &Memory {
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

    fn resolve(&self, profile: &str) -> Option<ProfileStamp> {
        (profile == "env").then(|| ProfileStamp {
            id: PacketProfileId::new("env").unwrap(),
            incarnation: 4,
            revision: *self.revision.borrow(),
        })
    }

    async fn export(
        &self,
        profile: &str,
        out: &mut [u8],
    ) -> Result<(ProfileStamp, usize), BuildFailure> {
        if let Some(failure) = *self.fail.borrow() {
            return Err(failure);
        }
        let stamp = self.resolve(profile).ok_or(BuildFailure::ProfileChanged)?;
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
    state: ConnectivityState<NoopRawMutex, 3, 2>,
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

type Svc<'a, 'e> =
    ConnectivityService<'a, NoopRawMutex, &'a Memory, &'e Exporter<'e>, 3, 2, 2, 1024>;

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

fn records(service: &Svc<'_, '_>, action: u32, args: &[(u32, Value)]) -> Vec<Vec<(u32, Value)>> {
    call(service, action, args)
        .unwrap()
        .records()
        .iter()
        .map(|record| record.iter().cloned().collect())
        .collect()
}

fn add_route(service: &Svc<'_, '_>, id: &str, endpoint: &str) -> Result<(), ActionError> {
    call(
        service,
        action::ROUTE_ADD,
        &[(1, text(id)), (2, text("env")), (3, text(endpoint))],
    )
    .map(|_| ())
}

#[test]
fn the_eight_actions_have_the_documented_arguments_and_access() {
    let actions: Vec<_> = Svc::ACTIONS
        .iter()
        .map(|a| (a.name, a.access, a.args.len(), a.stream))
        .collect();
    assert_eq!(
        actions,
        [
            ("route_add", ActionAccess::Mutate, 3, false),
            ("route_remove", ActionAccess::Mutate, 1, false),
            ("set_interval", ActionAccess::Mutate, 2, false),
            ("fallback_add", ActionAccess::Mutate, 3, false),
            ("fallback_remove", ActionAccess::Mutate, 2, false),
            ("route_list", ActionAccess::Read, 1, false),
            ("fallback_list", ActionAccess::Read, 2, false),
            #[cfg(feature = "packet-export")]
            ("packet_get", ActionAccess::Read, 1, true),
        ]
    );
    #[cfg(not(feature = "packet-export"))]
    assert!(Svc::ACTIONS.iter().all(|a| a.name != "packet_get"));
    // No route_status and no profile editing: health is a Status.
    assert!(Svc::ACTIONS.iter().all(|a| a.name != "route_status"));
    assert_eq!(Svc::NAME, "connectivity");
}

#[test]
fn a_route_needs_an_existing_profile_and_binds_its_incarnation() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    assert_eq!(
        call(
            &service,
            action::ROUTE_ADD,
            &[
                (1, text("r")),
                (2, text("nope")),
                (3, text("http://a.example"))
            ],
        )
        .unwrap_err(),
        ActionError::Rejected("profile_not_found")
    );
    assert!(fixture.memory.stored.borrow().routes.is_empty());
    add_route(&service, "r", "http://a.example").unwrap();
    let stored = fixture.memory.stored.borrow();
    let route = stored.get("r").unwrap();
    assert_eq!(route.profile.incarnation, 4);
    assert_eq!(route.interval_s, 60);
    assert!(route.fallbacks.is_empty());
}

#[test]
fn a_route_is_listed_back_completely_with_a_64_byte_endpoint() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    let endpoint = std::format!("http://{}.example.org:8080", "h".repeat(40));
    assert_eq!(endpoint.len(), 64);
    add_route(&service, "community", &endpoint).unwrap();
    let list = records(&service, action::ROUTE_LIST, &[(1, Value::U64(0))]);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0][0].1.as_str(), Some("community"));
    assert_eq!(list[0][1].1.as_str(), Some("env"));
    assert_eq!(
        list[0][2].1.as_str(),
        Some(endpoint.as_str()),
        "not truncated"
    );
    assert_eq!(list[0][3].1.as_u64(), Some(60));
    assert!(records(&service, action::ROUTE_LIST, &[(1, Value::U64(1))]).is_empty());
}

#[test]
fn fallbacks_keep_their_priority_and_the_interval_is_the_only_in_place_edit() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    add_route(&service, "r", "http://primary.example").unwrap();
    for (id, host) in [("one", "http://one.example"), ("two", "http://two.example")] {
        call(
            &service,
            action::FALLBACK_ADD,
            &[(1, text("r")), (2, text(id)), (3, text(host))],
        )
        .unwrap();
    }
    assert_eq!(
        call(
            &service,
            action::FALLBACK_ADD,
            &[
                (1, text("r")),
                (2, text("three")),
                (3, text("http://three.example"))
            ],
        )
        .unwrap_err(),
        ActionError::Rejected("too_many_fallbacks")
    );
    assert_eq!(
        call(
            &service,
            action::FALLBACK_ADD,
            &[
                (1, text("r")),
                (2, text("x")),
                (3, text("http://primary.example"))
            ],
        )
        .unwrap_err(),
        ActionError::Rejected("duplicate_endpoint"),
        "the endpoint of the primary is refused first"
    );
    let list = records(
        &service,
        action::FALLBACK_LIST,
        &[(1, text("r")), (2, Value::U64(0))],
    );
    let priorities: Vec<_> = list.iter().map(|r| r[2].1.as_u64().unwrap()).collect();
    let ids: Vec<String> = list
        .iter()
        .map(|r| r[0].1.as_str().unwrap().into())
        .collect();
    assert_eq!(
        (priorities, ids),
        (vec![1, 2], vec!["one".to_owned(), "two".to_owned()])
    );

    call(
        &service,
        action::SET_INTERVAL,
        &[(1, text("r")), (2, Value::U64(120))],
    )
    .unwrap();
    assert_eq!(
        fixture.memory.stored.borrow().get("r").unwrap().interval_s,
        120
    );
    for bad in [5u64, 86_401, u64::MAX] {
        assert_eq!(
            call(
                &service,
                action::SET_INTERVAL,
                &[(1, text("r")), (2, Value::U64(bad))]
            )
            .unwrap_err(),
            ActionError::Rejected("invalid_interval")
        );
    }
    call(
        &service,
        action::FALLBACK_REMOVE,
        &[(1, text("r")), (2, text("one"))],
    )
    .unwrap();
    assert_eq!(
        call(
            &service,
            action::FALLBACK_LIST,
            &[(1, text("nope")), (2, Value::U64(0))],
        )
        .unwrap_err(),
        ActionError::Rejected("route_not_found")
    );
}

#[test]
fn a_failed_commit_or_an_unreadable_store_changes_nothing() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    add_route(&service, "r", "http://a.example").unwrap();
    let before = fixture.memory.stored.borrow().clone();
    *fixture.memory.fail_commit.borrow_mut() = true;
    assert_eq!(
        add_route(&service, "s", "http://b.example").unwrap_err(),
        ActionError::Internal
    );
    assert_eq!(*fixture.memory.stored.borrow(), before);
    *fixture.memory.fail_commit.borrow_mut() = false;
    *fixture.memory.unreadable.borrow_mut() = true;
    assert_eq!(
        add_route(&service, "s", "http://b.example").unwrap_err(),
        ActionError::Unavailable
    );
}

#[test]
fn status_has_static_route_slots_and_no_content() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    // The schema is valid: 3 slots of 6 fields and 2 aggregates.
    assert_eq!(<Svc as Status>::STATUS_SCHEMA.len(), 20); // 3 route slots
    assert!(<Svc as Status>::STATUS_SCHEMA.len() <= rohi_service::MAX_FIELDS);
    let summary = block_on(service.summary());
    assert_eq!(
        summary.state,
        ServiceState::Stopped,
        "no route is a valid state"
    );
    fixture.state.mark_unreadable();
    assert_eq!(block_on(service.summary()).state, ServiceState::Failed);
}

#[cfg(feature = "packet-export")]
fn packet(service: &Svc<'_, '_>, profile: &str) -> Result<(u32, u64), ActionError> {
    match call(service, action::PACKET_GET, &[(1, text(profile))])? {
        ActionResult::Stream(info) => {
            assert_eq!(info.files.len(), 1);
            assert_eq!(info.files[0].name.as_str(), "packet.bin");
            Ok((info.id, info.files[0].size_bytes))
        }
        ActionResult::Records(_) => panic!("packet_get returns a stream"),
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
fn packet_get_streams_one_immutable_packet_without_any_route() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    assert!(fixture.memory.stored.borrow().routes.is_empty());
    let (stream, size) = packet(&service, "env").unwrap();
    assert_eq!(size, 700);

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
fn concurrent_streams_are_bounded_and_never_overwrite_each_other() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    let (a, _) = packet(&service, "env").unwrap();
    let (b, _) = packet(&service, "env").unwrap();
    assert_ne!(a, b);
    assert_eq!(packet(&service, "env").unwrap_err(), ActionError::Busy);
    // Each stream keeps its own bytes.
    assert_ne!(
        read(&service, a, 0).unwrap().0,
        read(&service, b, 0).unwrap().0
    );
    // A closed stream makes room.
    let registry = (&service,);
    block_on(registry.close_stream(Svc::ID, a)).unwrap();
    assert!(packet(&service, "env").is_ok());
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
    let (stream, _) = packet(&service, "env").unwrap();
    *fixture.revision.borrow_mut() = 2;
    assert_eq!(
        read(&service, stream, 0).unwrap_err(),
        ActionError::UnknownStream
    );
    assert_eq!(service.open_streams(), 0, "the revoked packet was released");

    assert_eq!(
        packet(&service, "nope").unwrap_err(),
        ActionError::Rejected("profile_not_found")
    );
    *fixture.fail.borrow_mut() = Some(BuildFailure::TimeUnavailable);
    assert_eq!(
        packet(&service, "env").unwrap_err(),
        ActionError::Rejected("time_unavailable")
    );
    *fixture.fail.borrow_mut() = Some(BuildFailure::NothingToPackage);
    assert_eq!(
        packet(&service, "env").unwrap_err(),
        ActionError::Rejected("nothing_to_package")
    );
    assert_eq!(service.open_streams(), 0);
}

#[cfg(feature = "packet-export")]
#[test]
fn an_abandoned_stream_expires_and_makes_room() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    let (a, _) = packet(&service, "env").unwrap();
    let (_b, _) = packet(&service, "env").unwrap();
    assert_eq!(packet(&service, "env").unwrap_err(), ActionError::Busy);

    *fixture.now.borrow_mut() = EXPORT_TTL_MS - 1;
    assert!(read(&service, a, 0).is_ok(), "still within its lifetime");
    *fixture.now.borrow_mut() = EXPORT_TTL_MS;
    assert_eq!(
        read(&service, a, 0).unwrap_err(),
        ActionError::UnknownStream
    );
    assert_eq!(service.open_streams(), 0);
    assert!(packet(&service, "env").is_ok());
}

#[cfg(feature = "packet-export")]
#[test]
fn an_expired_stream_does_not_block_a_new_packet_get() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    // One slot only: the abandoned stream holds all of the capacity.
    let service: ConnectivityService<'_, NoopRawMutex, &Memory, &Exporter<'_>, 3, 2, 1, 1024> =
        ConnectivityService::new(&fixture.memory, &exporter, &fixture.state);
    let registry = (&service,);
    let open =
        |service: &ConnectivityService<'_, NoopRawMutex, &Memory, &Exporter<'_>, 3, 2, 1, 1024>| {
            block_on((service,).invoke(Svc::ID, action::PACKET_GET, &[(1, text("env"))]))
        };
    assert!(open(&service).is_ok());
    assert_eq!(open(&service).unwrap_err(), ActionError::Busy);

    // Past its lifetime, with no read of the old stream in between.
    *fixture.now.borrow_mut() = EXPORT_TTL_MS;
    assert!(open(&service).is_ok(), "the new request reclaims the slot");
    assert_eq!(service.open_streams(), 1);
    registry.link_lost();
}

fn failing_view(state: RouteState) -> crate::router::RouteView {
    crate::router::RouteView {
        id: crate::RouteId::new("r").unwrap(),
        state,
        queue_depth: 1,
        counters: crate::router::RouteCounters::default(),
        active_path: None,
    }
}

#[test]
fn failing_deliveries_degrade_the_health_even_when_packets_are_built() {
    let fixture = Fixture::new();
    let exporter = fixture.exporter();
    let service = Svc::new(&fixture.memory, &exporter, &fixture.state);
    let mut routes = RouteSet::default();
    routes
        .add_route(
            "r",
            ProfileBinding {
                id: PacketProfileId::new("env").unwrap(),
                incarnation: 4,
            },
            "http://a.example",
        )
        .unwrap();
    fixture.state.publish(&routes);

    // Delivering normally: healthy.
    let mut views = heapless::Vec::new();
    views.push(failing_view(RouteState::Delivering)).unwrap();
    fixture.state.report(views, crate::state::Health::default());
    assert_eq!(block_on(service.summary()).health, ServiceHealth::Ok);

    // Every path is failing and cooling down: degraded, with an issue.
    let mut views = heapless::Vec::new();
    views.push(failing_view(RouteState::Backoff)).unwrap();
    fixture.state.report(views, crate::state::Health::default());
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
        assert_eq!(<Slots<1> as RouteSlots>::SCHEMA.len(), 2 + 6);
        assert_eq!(<Slots<2> as RouteSlots>::SCHEMA.len(), 2 + 2 * 6);
        assert_eq!(<Slots<3> as RouteSlots>::SCHEMA.len(), 2 + 3 * 6);
        let two = names::<2>();
        assert_eq!(&two[..2], ["active_routes", "total_queue_depth"]);
        assert!(two.contains(&"route1.active_path"));
        assert!(!two.contains(&"route2.id"));
    }

    #[test]
    fn ids_names_and_kinds_are_stable() {
        let schema = <Slots<2> as RouteSlots>::SCHEMA;
        let find =
            |name: &str| -> FieldDescriptor { *schema.iter().find(|f| f.name == name).unwrap() };
        assert_eq!(find("active_routes").id, status_field::ACTIVE_ROUTES);
        assert_eq!(find("route0.id").id, 10);
        assert_eq!(find("route0.id").kind, ValueKind::String);
        assert_eq!(find("route0.state").id, 11);
        assert_eq!(find("route0.state").kind, ValueKind::Enum);
        assert_eq!(find("route1.queue_depth").id, 22);
        assert_eq!(find("route1.active_path").id, 25);
        assert_eq!(find("route1.delivered").kind, ValueKind::U64);
        // Ids are unique.
        let mut ids: std::vec::Vec<u32> = schema.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), schema.len());
    }

    #[test]
    fn the_service_of_two_routes_exposes_exactly_that_schema() {
        use embassy_sync::blocking_mutex::raw::NoopRawMutex;
        use rohi_service::Status;

        use crate::managed::ConnectivityService;

        struct Never;
        impl crate::ConnectivityStore<2, 1> for Never {
            async fn load(&self) -> Result<crate::Routes<2, 1>, crate::StoreError> {
                Err(crate::StoreError::Unavailable)
            }
            async fn commit(&self, _: &crate::Routes<2, 1>) -> Result<(), crate::StoreError> {
                Err(crate::StoreError::Unavailable)
            }
        }
        impl crate::PacketExporter for Never {
            fn now_ms(&self) -> u64 {
                0
            }
            fn resolve(&self, _: &str) -> Option<rohi_telemetry::ProfileStamp> {
                None
            }
            async fn export(
                &self,
                _: &str,
                _: &mut [u8],
            ) -> Result<(rohi_telemetry::ProfileStamp, usize), crate::BuildFailure> {
                Err(crate::BuildFailure::ProfileChanged)
            }
        }
        type Svc<'a> = ConnectivityService<'a, NoopRawMutex, Never, Never, 2, 1, 1, 64>;
        assert_eq!(<Svc<'_> as Status>::STATUS_SCHEMA.len(), 14);
    }
}
