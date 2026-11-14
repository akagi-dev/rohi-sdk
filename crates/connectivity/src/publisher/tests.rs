use core::cell::{Cell, RefCell};
use std::string::String;
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_hal_async::delay::DelayNs;
use rohi_telemetry::PacketProfileId;

use super::*;
use crate::config::ProfileBinding;
use crate::endpoint::Endpoint;
use crate::transport::{TransportAck, TransportError};

struct Clk<'a>(&'a Cell<u64>);

impl Clock for Clk<'_> {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(self.0.get())
    }
}

struct Never;

impl DelayNs for Never {
    async fn delay_ns(&mut self, _: u32) {
        core::future::pending::<()>().await
    }
}

#[derive(Default)]
struct World {
    revision: Cell<u32>,
    captures: Cell<u32>,
    builds: Cell<u32>,
    fail_build: Cell<Option<BuildFailure>>,
    sent: RefCell<Vec<(String, Vec<u8>)>>,
    // Hosts that answer with a status other than 202.
    answers: RefCell<Vec<(&'static str, u16)>>,
}

struct Provider<'a>(&'a World);

impl PacketProvider for Provider<'_> {
    type Snapshot = u32;

    fn resolve(&self, profile: &str) -> Option<ProfileStamp> {
        (profile == "env").then(|| ProfileStamp {
            id: PacketProfileId::new("env").unwrap(),
            incarnation: 1,
            revision: self.0.revision.get(),
        })
    }

    fn is_current(&self, stamp: &ProfileStamp) -> bool {
        self.resolve(stamp.id.as_str()).as_ref() == Some(stamp)
    }

    async fn capture(&mut self) -> Result<u32, BuildFailure> {
        self.0.captures.set(self.0.captures.get() + 1);
        Ok(self.0.captures.get())
    }

    async fn build(
        &mut self,
        snapshot: &u32,
        stamp: &ProfileStamp,
        out: &mut [u8],
    ) -> Result<usize, BuildFailure> {
        if let Some(failure) = self.0.fail_build.get() {
            return Err(failure);
        }
        if !self.is_current(stamp) {
            return Err(BuildFailure::ProfileChanged);
        }
        self.0.builds.set(self.0.builds.get() + 1);
        let text = std::format!("packet-{}-{}", stamp.revision, snapshot);
        out[..text.len()].copy_from_slice(text.as_bytes());
        Ok(text.len())
    }

    async fn changed(&self) {
        core::future::pending::<()>().await
    }
}

struct Transport<'a>(&'a World);

impl PacketTransport for Transport<'_> {
    async fn deliver(
        &mut self,
        endpoint: Endpoint<'_>,
        packet: &[u8],
    ) -> Result<TransportAck, TransportError> {
        self.0
            .sent
            .borrow_mut()
            .push((endpoint.host.into(), packet.to_vec()));
        let status = self
            .0
            .answers
            .borrow()
            .iter()
            .find(|(host, _)| *host == endpoint.host)
            .map_or(202, |(_, status)| *status);
        crate::classify_status(status)
    }
}

type P<'a, const SLOTS: usize> =
    Publisher<'a, NoopRawMutex, Provider<'a>, Transport<'a>, Never, Clk<'a>, 2, SLOTS, 32>;

fn binding() -> ProfileBinding {
    ProfileBinding {
        id: PacketProfileId::new("env").unwrap(),
        incarnation: 1,
    }
}

fn publisher<'a, const SLOTS: usize>(
    world: &'a World,
    state: &'a ConnectivityState<NoopRawMutex>,
    clock: &'a Cell<u64>,
) -> P<'a, SLOTS> {
    Publisher::new(state, Provider(world), Transport(world), Never, Clk(clock))
}

fn hosts(world: &World) -> Vec<String> {
    world
        .sent
        .borrow()
        .iter()
        .map(|(host, _)| host.clone())
        .collect()
}

#[test]
fn routes_due_together_share_one_capture_and_one_sealed_packet() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(), "http://a.example")
        .unwrap();
    config
        .add_route("b", binding(), "http://b.example")
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());
    // Both routes published at the same instant: one capture, one build.
    assert_eq!((world.captures.get(), world.builds.get()), (1, 1));
    // One delivery per step; the second step delivers the other route.
    block_on(publisher.step());
    let sent = world.sent.borrow();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].1, sent[1].1, "the very same bytes");
    assert_eq!(sent[0].1, b"packet-0-1");
    drop(sent);
    assert_eq!(
        publisher.router().pool().in_use(),
        0,
        "all references released"
    );
    assert_eq!(state.views().len(), 2);
}

#[test]
fn each_route_keeps_its_own_cadence() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .add_route("fast", binding(), "http://fast.example")
        .unwrap();
    config
        .add_route("slow", binding(), "http://slow.example")
        .unwrap();
    config.set_interval("fast", 30).unwrap();
    config.set_interval("slow", 300).unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    for second in [0, 0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300] {
        clock.set(second * 1_000);
        block_on(publisher.step());
        block_on(publisher.step());
    }
    let fast = hosts(&world)
        .iter()
        .filter(|h| *h == "fast.example")
        .count();
    let slow = hosts(&world)
        .iter()
        .filter(|h| *h == "slow.example")
        .count();
    assert_eq!((fast, slow), (11, 2), "0, 30 .. 300 versus 0 and 300");
    // Different instants are different captures: nothing is shared across them.
    assert!(world.captures.get() >= 11);
}

#[test]
fn a_failing_primary_hands_the_same_bytes_to_the_fallback() {
    let world = World::default();
    world.answers.borrow_mut().push(("primary.example", 503));
    let state = ConnectivityState::<NoopRawMutex>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(), "http://primary.example")
        .unwrap();
    config
        .add_fallback("a", "backup", "http://backup.example")
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step()); // publish + first attempt (primary, 503)
    block_on(publisher.step()); // the fallback, at once
    assert_eq!(hosts(&world), ["primary.example", "backup.example"]);
    let sent = world.sent.borrow();
    assert_eq!(sent[0].1, sent[1].1);
    drop(sent);
    let view = &state.views()[0];
    assert_eq!(view.counters.delivered, 1);
    assert_eq!(view.queue_depth, 0);
}

#[test]
fn a_policy_change_invalidates_queued_packets_before_anything_is_sent() {
    let world = World::default();
    world.answers.borrow_mut().push(("a.example", 503));
    let state = ConnectivityState::<NoopRawMutex>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(), "http://a.example")
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);
    block_on(publisher.step()); // queued, attempt fails with 503
    assert_eq!(world.sent.borrow().len(), 1);
    assert_eq!(state.views()[0].queue_depth, 1);

    // The policy of the profile changes: the queued packet must not be sent.
    world.revision.set(1);
    clock.set(10_000);
    block_on(publisher.step());
    let sends_of_old: usize = world
        .sent
        .borrow()
        .iter()
        .filter(|(_, bytes)| bytes.starts_with(b"packet-0"))
        .count();
    assert_eq!(sends_of_old, 1, "no new send under the revoked policy");
    assert_eq!(state.views()[0].counters.invalidated, 1);
}

#[test]
fn a_production_failure_is_a_diagnostic_not_a_transport_error_and_does_not_stop_the_cadence() {
    let world = World::default();
    world.fail_build.set(Some(BuildFailure::TimeUnavailable));
    let state = ConnectivityState::<NoopRawMutex>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(), "http://a.example")
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());
    assert!(world.sent.borrow().is_empty());
    assert_eq!(state.health().last_build_failure, Some("time_unavailable"));
    assert_eq!(state.views()[0].counters.delivered, 0);

    // The next publication happens at the interval and succeeds once fixed.
    world.fail_build.set(None);
    clock.set(60_000);
    block_on(publisher.step());
    assert_eq!(world.sent.borrow().len(), 1);
    assert_eq!(state.health().last_build_failure, None);
}

#[test]
fn an_exhausted_pool_is_counted_and_other_work_continues() {
    let world = World::default();
    // Always fail the transport so packets stay queued and hold their slot.
    world.answers.borrow_mut().push(("a.example", 503));
    world.answers.borrow_mut().push(("b.example", 503));
    let state = ConnectivityState::<NoopRawMutex>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(), "http://a.example")
        .unwrap();
    state.publish(&config);
    let mut publisher: P<1> = publisher(&world, &state, &clock);
    block_on(publisher.step());
    assert_eq!(publisher.router().pool().in_use(), 1);

    // A second route needs another packet instant: no slot is free.
    config
        .add_route("b", binding(), "http://b.example")
        .unwrap();
    state.publish(&config);
    block_on(publisher.step());
    let exhausted: u32 = state
        .views()
        .iter()
        .map(|v| v.counters.pool_exhausted)
        .sum();
    assert!(exhausted >= 1);
}

#[test]
fn no_route_means_nothing_is_captured_or_sent() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex>::new();
    let clock = Cell::new(0);
    let mut publisher: P<4> = publisher(&world, &state, &clock);
    block_on(publisher.step());
    assert_eq!((world.captures.get(), world.sent.borrow().len()), (0, 0));
}

#[test]
fn removing_a_route_stops_its_deliveries_and_frees_its_packets() {
    let world = World::default();
    world.answers.borrow_mut().push(("a.example", 503));
    let state = ConnectivityState::<NoopRawMutex>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(), "http://a.example")
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);
    block_on(publisher.step());
    assert_eq!(publisher.router().pool().in_use(), 1);
    config.remove_route("a").unwrap();
    state.publish(&config);
    clock.set(100_000);
    block_on(publisher.step());
    assert_eq!(publisher.router().pool().in_use(), 0);
    assert_eq!(world.sent.borrow().len(), 1);
}

#[test]
fn an_unreadable_store_leaves_no_active_route() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(), "http://a.example")
        .unwrap();
    state.publish(&config);
    state.mark_unreadable();
    assert!(state.is_unreadable());
    assert!(state.routes().routes.is_empty());
    let mut publisher: P<4> = publisher(&world, &state, &clock);
    block_on(publisher.step());
    assert!(world.sent.borrow().is_empty());
}
