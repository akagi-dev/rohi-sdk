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
use core::cell::{Cell, RefCell};
use std::string::String;
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_hal_async::delay::DelayNs;

use super::*;

// The capacities the tests exercise: more than a device keeps, to cover every edge.
type RouteSet = crate::Routes<3>;
use crate::testing::{TestPolicy, status};
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
    // The profile slots that are gone.
    unavailable: Cell<bool>,
    unavailable_on_capture: Cell<bool>,
    revision: Cell<u32>,
    captures: Cell<u32>,
    builds: Cell<u32>,
    fail_build: Cell<Option<BuildFailure>>,
    routes_on_capture: RefCell<Option<RouteSet>>,
    routes_on_build: RefCell<Option<RouteSet>>,
    routes_on_delivery: RefCell<Option<RouteSet>>,
    // Published while the attempt runs; the attempt still answers.
    routes_after_delivery: RefCell<Option<RouteSet>>,
    sent: RefCell<Vec<(String, Vec<u8>)>>,
    // Hosts that answer with a status other than 202.
    answers: RefCell<Vec<(&'static str, u16)>>,
}

struct Provider<'a> {
    world: &'a World,
    state: &'a ConnectivityState<NoopRawMutex, 3>,
}

impl PacketProvider for Provider<'_> {
    type Snapshot = u32;

    // The device has two profile slots; a slot has its own revision.
    fn resolve(&self, profile_index: u8) -> Option<ProfileStamp> {
        (profile_index < 2 && !self.world.unavailable.get()).then(|| ProfileStamp {
            profile_index,
            revision: self.world.revision.get() + u32::from(profile_index) * 100,
        })
    }

    fn is_current(&self, stamp: &ProfileStamp) -> bool {
        self.resolve(stamp.profile_index) == Some(*stamp)
    }

    async fn capture(&mut self) -> Result<u32, BuildFailure> {
        if self.world.unavailable_on_capture.get() {
            // The profile slot disappears while the capture runs.
            self.world.unavailable.set(true);
        }
        self.world.captures.set(self.world.captures.get() + 1);
        if let Some(routes) = self.world.routes_on_capture.borrow_mut().take() {
            self.state.publish(&routes);
        }
        Ok(self.world.captures.get())
    }

    async fn build(
        &mut self,
        snapshot: &u32,
        stamp: &ProfileStamp,
        out: &mut [u8],
    ) -> Result<usize, BuildFailure> {
        if let Some(failure) = self.world.fail_build.get() {
            return Err(failure);
        }
        if !self.is_current(stamp) {
            return Err(BuildFailure::ProfileChanged);
        }
        self.world.builds.set(self.world.builds.get() + 1);
        let text = std::format!("packet-{}-{}", stamp.revision, snapshot);
        out[..text.len()].copy_from_slice(text.as_bytes());
        if let Some(routes) = self.world.routes_on_build.borrow_mut().take() {
            self.state.publish(&routes);
        }
        Ok(text.len())
    }

    async fn changed(&self) {
        core::future::pending::<()>().await
    }
}

struct Transport<'a> {
    world: &'a World,
    state: &'a ConnectivityState<NoopRawMutex, 3>,
}

impl PacketTransport for Transport<'_> {
    fn handles(&self, scheme: &str) -> bool {
        scheme == "http"
    }

    async fn deliver(
        &mut self,
        endpoint: &str,
        packet: &[u8],
    ) -> Result<TransportAck, TransportError> {
        let routes = { self.world.routes_on_delivery.borrow_mut().take() };
        if let Some(routes) = routes {
            self.state.publish(&routes);
            return core::future::pending().await;
        }
        if let Some(routes) = self.world.routes_after_delivery.borrow_mut().take() {
            self.state.publish(&routes);
        }
        let host = endpoint
            .strip_prefix("http://")
            .and_then(|rest| rest.split(':').next())
            .unwrap_or(endpoint);
        self.world
            .sent
            .borrow_mut()
            .push((host.into(), packet.to_vec()));
        let code = self
            .world
            .answers
            .borrow()
            .iter()
            .find(|(answers_host, _)| *answers_host == host)
            .map_or(202, |(_, status)| *status);
        status(code)
    }
}

type P<'a, const SLOTS: usize> =
    Publisher<'a, NoopRawMutex, Provider<'a>, Transport<'a>, Never, Clk<'a>, 3, 2, SLOTS, 32>;

fn publisher<'a, const SLOTS: usize>(
    world: &'a World,
    state: &'a ConnectivityState<NoopRawMutex, 3>,
    clock: &'a Cell<u64>,
) -> P<'a, SLOTS> {
    Publisher::new(
        state,
        Provider { world, state },
        Transport { world, state },
        Never,
        Clk(clock),
    )
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
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    config
        .set_endpoint(1, 0, Some("http://b.example"), &TestPolicy)
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
    assert_eq!(state.views().iter().flatten().count(), 2);
}

#[test]
fn each_route_keeps_its_own_cadence() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://fast.example"), &TestPolicy)
        .unwrap();
    config
        .set_endpoint(1, 0, Some("http://slow.example"), &TestPolicy)
        .unwrap();
    config.set_interval(0, 30).unwrap();
    config.set_interval(1, 300).unwrap();
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
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://primary.example"), &TestPolicy)
        .unwrap();
    config
        .set_fallback(0, Some("http://backup.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step()); // publish + first attempt (primary, 503)
    block_on(publisher.step()); // the fallback, at once
    assert_eq!(hosts(&world), ["primary.example", "backup.example"]);
    let sent = world.sent.borrow();
    assert_eq!(sent[0].1, sent[1].1);
    drop(sent);
    let view = state.views()[0].unwrap();
    assert_eq!(view.counters.delivered, 1);
    assert_eq!(view.queue_depth, 0);
}

#[test]
fn a_policy_change_invalidates_queued_packets_before_anything_is_sent() {
    let world = World::default();
    world.answers.borrow_mut().push(("a.example", 503));
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);
    block_on(publisher.step()); // queued, attempt fails with 503
    assert_eq!(world.sent.borrow().len(), 1);
    assert_eq!(state.views()[0].unwrap().queue_depth, 1);

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
    assert_eq!(state.views()[0].unwrap().counters.invalidated, 1);
}

#[test]
fn a_production_failure_is_a_diagnostic_not_a_transport_error_and_does_not_stop_the_cadence() {
    let world = World::default();
    world.fail_build.set(Some(BuildFailure::TimeUnavailable));
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());
    assert!(world.sent.borrow().is_empty());
    assert_eq!(state.health().last_build_failure, Some("time_unavailable"));
    assert_eq!(state.views()[0].unwrap().counters.delivered, 0);

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
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    let mut publisher: P<1> = publisher(&world, &state, &clock);
    block_on(publisher.step());
    assert_eq!(publisher.router().pool().in_use(), 1);

    // A second route needs another packet instant: no slot is free.
    config
        .set_endpoint(1, 0, Some("http://b.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    block_on(publisher.step());
    let exhausted: u32 = state
        .views()
        .iter()
        .flatten()
        .map(|v| v.counters.pool_exhausted)
        .sum();
    assert!(exhausted >= 1);
}

#[test]
fn no_route_means_nothing_is_captured_or_sent() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut publisher: P<4> = publisher(&world, &state, &clock);
    block_on(publisher.step());
    assert_eq!((world.captures.get(), world.sent.borrow().len()), (0, 0));
}

#[test]
fn removing_a_route_stops_its_deliveries_and_frees_its_packets() {
    let world = World::default();
    world.answers.borrow_mut().push(("a.example", 503));
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);
    block_on(publisher.step());
    assert_eq!(publisher.router().pool().in_use(), 1);
    config.set_endpoint(0, 0, None, &TestPolicy).unwrap();
    state.publish(&config);
    clock.set(100_000);
    block_on(publisher.step());
    assert_eq!(publisher.router().pool().in_use(), 0);
    assert_eq!(world.sent.borrow().len(), 1);
}

#[test]
fn an_unreadable_store_leaves_no_active_route() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    state.mark_unreadable();
    assert!(state.is_unreadable());
    assert!(state.routes().count() == 0);
    let mut publisher: P<4> = publisher(&world, &state, &clock);
    block_on(publisher.step());
    assert!(world.sent.borrow().is_empty());
}

#[test]
fn a_profile_that_disappears_during_the_capture_never_joins_the_execution() {
    let world = World::default();
    world.unavailable_on_capture.set(true);
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());
    // The capture finished while the profile slot was already gone: nothing is
    // built or sent for it.
    assert_eq!(world.builds.get(), 0);
    assert!(world.sent.borrow().is_empty());
    assert_eq!(publisher.router().pool().in_use(), 0);

    // The next execution suspends the route.
    clock.set(60_000);
    block_on(publisher.step());
    assert_eq!(
        state.views()[0].unwrap().state,
        crate::router::RouteState::ProfileMissing
    );
    assert_eq!(world.builds.get(), 0);
    assert!(world.sent.borrow().is_empty());
}

#[test]
fn removing_a_route_during_capture_does_not_publish_or_send_its_packet() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    world.routes_on_capture.replace(Some(RouteSet::default()));
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());

    assert_eq!(world.builds.get(), 0);
    assert!(world.sent.borrow().is_empty());
    assert_eq!(publisher.router().pool().in_use(), 0);
}

#[test]
fn changing_fallbacks_during_build_does_not_admit_a_stale_packet() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    config
        .set_fallback(0, Some("http://backup.example"), &TestPolicy)
        .unwrap();
    world.routes_on_build.replace(Some(config));
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());

    assert_eq!(world.builds.get(), 1);
    assert!(world.sent.borrow().is_empty());
    assert_eq!(publisher.router().pool().in_use(), 0);
}

#[test]
fn changing_fallbacks_cancels_an_in_flight_delivery() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    config
        .set_fallback(0, Some("http://backup.example"), &TestPolicy)
        .unwrap();
    world.routes_on_delivery.replace(Some(config));
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());

    assert!(world.sent.borrow().is_empty());
    assert_eq!(publisher.router().pool().in_use(), 0);
}

#[test]
fn replacing_the_endpoint_cancels_an_in_flight_delivery_and_sends_nothing_stale() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    config
        .set_endpoint(0, 0, Some("http://b.example"), &TestPolicy)
        .unwrap();
    world.routes_on_delivery.replace(Some(config));
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());

    assert!(world.sent.borrow().is_empty());
    assert_eq!(publisher.router().pool().in_use(), 0);
    assert_eq!(publisher.router().queued(), 0);
}

#[test]
fn editing_another_route_during_a_delivery_keeps_the_result_of_this_one() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    config
        .set_endpoint(1, 0, Some("http://b.example"), &TestPolicy)
        .unwrap();
    world.routes_after_delivery.replace(Some(config));
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());

    let route = state.views()[0].unwrap();
    assert_eq!(route.counters.delivered, 1);
    assert_eq!(
        route.queue_depth, 0,
        "the delivered packet is not sent again"
    );
    assert_eq!(hosts(&world), ["a.example"]);
}

#[test]
fn a_deleted_and_recreated_slot_starts_clean() {
    let world = World::default();
    world.answers.borrow_mut().push(("a.example", 503));
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);
    block_on(publisher.step());
    let old = state.views()[0].unwrap();
    assert_eq!((old.queue_depth, old.counters.retries), (1, 1));

    // Deleted and recreated with the same endpoint: nothing is inherited.
    config.set_endpoint(0, 0, None, &TestPolicy).unwrap();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    block_on(publisher.step());
    let new = state.views()[0].unwrap();
    assert_ne!(new.generation, old.generation);
    assert_eq!(new.generation, config.get(0).unwrap().generation);
    assert_eq!(new.counters.retries, 1, "only the attempt of the new route");
    assert_eq!(new.counters.published, 1);
    let sent = world.sent.borrow();
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[0].1, sent[1].1, "a fresh packet, not the old one");
}

#[test]
fn updating_slot_zero_never_changes_slot_one_and_each_profile_slot_has_its_own_packet() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    config
        .set_endpoint(1, 1, Some("http://b.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());
    block_on(publisher.step());
    // Two profile slots: one capture, two builds, two different packets.
    assert_eq!((world.captures.get(), world.builds.get()), (1, 2));
    {
        let sent = world.sent.borrow();
        assert_eq!(sent[0].1, b"packet-0-1");
        assert_eq!(sent[1].1, b"packet-100-1");
    }
    let before = state.views()[1].unwrap();

    config
        .set_endpoint(0, 1, Some("http://c.example"), &TestPolicy)
        .unwrap();
    config.set_interval(0, 120).unwrap();
    state.publish(&config);
    clock.set(1);
    block_on(publisher.step());
    assert_eq!(state.views()[1].unwrap(), before);
}

#[test]
fn a_profile_slot_that_does_not_exist_suspends_its_route_only() {
    let world = World::default();
    let state = ConnectivityState::<NoopRawMutex, 3>::new();
    let clock = Cell::new(0);
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 5, Some("http://a.example"), &TestPolicy)
        .unwrap();
    config
        .set_endpoint(1, 0, Some("http://b.example"), &TestPolicy)
        .unwrap();
    state.publish(&config);
    let mut publisher: P<4> = publisher(&world, &state, &clock);

    block_on(publisher.step());
    assert_eq!(hosts(&world), ["b.example"]);
    assert_eq!(
        state.views()[0].unwrap().state,
        crate::router::RouteState::ProfileMissing
    );
}
