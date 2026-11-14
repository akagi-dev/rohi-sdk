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
use crate::provider::BuildFailure;

#[derive(Default)]
struct Memory {
    stored: RefCell<RouteSet>,
    fail_commit: RefCell<bool>,
    unreadable: RefCell<bool>,
    commits: RefCell<u32>,
}

impl ConnectivityStore for &Memory {
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
}

impl PacketExporter for &Exporter<'_> {
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
    state: ConnectivityState<NoopRawMutex>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            memory: Memory::default(),
            revision: RefCell::new(1),
            fail: RefCell::new(None),
            builds: RefCell::new(0),
            state: ConnectivityState::new(),
        }
    }

    fn exporter(&self) -> Exporter<'_> {
        Exporter {
            revision: &self.revision,
            fail: &self.fail,
            builds: &self.builds,
        }
    }
}

type Svc<'a, 'e> = ConnectivityService<'a, NoopRawMutex, &'a Memory, &'e Exporter<'e>, 2, 1024>;

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
            ("packet_get", ActionAccess::Read, 1, true),
        ]
    );
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
    assert_eq!(<Svc as Status>::STATUS_SCHEMA.len(), 20);
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

fn read(service: &Svc<'_, '_>, stream: u32, offset: u64) -> Result<(Vec<u8>, bool), ActionError> {
    let mut buf = [0u8; 512];
    let registry = (service,);
    let chunk = block_on(registry.read_stream(Svc::ID, stream, 1, offset, &mut buf))?;
    Ok((buf[..chunk.len].to_vec(), chunk.eof))
}

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
