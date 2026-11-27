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
use crate::testing::{TestPolicy, status};

// More slots than a device keeps, to cover every edge.
type RouteSet = crate::Routes<3>;

type R = Router<3, 2, 4, 16>;

fn t(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

fn stamp(revision: u32) -> ProfileStamp {
    ProfileStamp {
        profile_index: 0,
        revision,
    }
}

fn exists(index: u8) -> bool {
    index < 2
}

fn config() -> RouteSet {
    let mut config = RouteSet::default();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    config
}

fn packet(router: &mut R, revision: u32, bytes: &[u8]) -> PacketHandle {
    let (handle, buffer) = router.pool_mut().alloc(stamp(revision)).unwrap();
    buffer[..bytes.len()].copy_from_slice(bytes);
    assert!(router.pool_mut().seal(handle, bytes.len()));
    handle
}

fn ok() -> Result<TransportAck, TransportError> {
    Ok(crate::testing::ACCEPTED)
}

fn view(router: &R, config: &RouteSet, index: usize, now: u64) -> RouteView {
    router.views(config, t(now))[index].unwrap()
}

#[test]
fn a_new_route_is_due_at_once_then_every_interval_and_unrelated_edits_do_not_move_it() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, exists, t(1_000));
    assert_eq!(router.due(t(1_000)).as_slice(), [0]);

    let handle = packet(&mut router, 1, b"x");
    assert_eq!(router.admit(0, handle, t(1_000)), Admit::Queued);
    router.pool_mut().release(handle);
    assert!(router.due(t(60_999)).is_empty());
    assert_eq!(router.due(t(61_000)).as_slice(), [0]);
    // A sync with an unchanged configuration keeps the deadline.
    router.sync(&config, exists, t(30_000));
    // The queued packet is deliverable now: that is the earliest wakeup.
    assert_eq!(router.next_wakeup(&config), Some(t(0)));
    let d = router.next_delivery(&config, t(1_000)).unwrap();
    router.complete(d, ok(), t(1_000));
    assert_eq!(router.next_wakeup(&config), Some(t(61_000)));
}

#[test]
fn empty_slots_have_no_runtime_view_or_wakeup() {
    let config = RouteSet::default();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    assert!(router.due(t(1_000_000)).is_empty());
    assert_eq!(router.next_wakeup(&config), None);
    assert!(router.next_delivery(&config, t(0)).is_none());
    assert!(router.views(&config, t(0)).iter().all(Option::is_none));
}

#[test]
fn an_interval_edit_rebases_the_deadline_without_clearing_the_queue_or_publishing_extra() {
    let mut config = config();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);

    config.set_interval(0, 120).unwrap();
    router.sync(&config, exists, t(10_000));
    assert!(router.due(t(10_000)).is_empty(), "no extra frame");
    assert!(router.due(t(129_999)).is_empty());
    assert_eq!(router.due(t(130_000)).as_slice(), [0]);
    assert_eq!(router.queued(), 1, "the queue is kept");
}

#[test]
fn only_202_completes_a_packet_and_the_same_bytes_are_retried() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"signed-bytes");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);

    let first = router.next_delivery(&config, t(0)).unwrap();
    router.complete(first, Err(TransportError::Timeout), t(0));
    // Cooling down: nothing to send until the backoff ends.
    assert!(router.next_delivery(&config, t(4_999)).is_none());
    let again = router.next_delivery(&config, t(5_000)).unwrap();
    assert_eq!(again.packet, first.packet);
    assert_eq!(router.pool().get(again.packet).unwrap().1, b"signed-bytes");
    // A 200 is not an acknowledgement.
    router.complete(again, crate::testing::status(200), t(5_000));
    assert_eq!(router.queued(), 1);
    let third = router.next_delivery(&config, t(15_000)).unwrap();
    router.complete(third, ok(), t(15_000));
    assert_eq!(router.queued(), 0);
    assert_eq!(router.pool().in_use(), 0, "every reference was released");
    let view = view(&router, &config, 0, 15_000);
    assert_eq!((view.counters.delivered, view.counters.retries), (1, 2));
}

fn with_fallback() -> RouteSet {
    let mut config = config();
    config
        .set_fallback(0, Some("http://one.example"), &TestPolicy)
        .unwrap();
    config
}

#[test]
fn a_failing_primary_fails_over_at_once_and_a_fallback_ack_completes_the_route() {
    let config = with_fallback();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"p");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);

    let d = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(d.path, 0);
    router.complete(d, status(503), t(0));
    let d = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(d.path, 1, "the fallback, without waiting for the primary");
    router.complete(d, ok(), t(0));

    assert_eq!(router.queued(), 0);
    let view = view(&router, &config, 0, 0);
    assert_eq!((view.counters.delivered, view.counters.failovers), (1, 1));
}

#[test]
fn without_a_fallback_there_is_only_the_primary_path() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"p");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, status(503), t(0));
    assert!(router.next_delivery(&config, t(0)).is_none());
    assert_eq!(router.next_delivery(&config, t(5_000)).unwrap().path, 0);
}

#[test]
fn the_primary_is_preferred_again_when_its_cooldown_ends() {
    let config = with_fallback();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    for round in 0..2u64 {
        let handle = packet(&mut router, 1, b"p");
        router.admit(0, handle, t(round * 100_000));
        router.pool_mut().release(handle);
    }
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Timeout), t(0));
    let d = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(d.path, 1);
    router.complete(d, ok(), t(0));
    // The next packet: still within the primary's cooldown it goes to the
    // fallback, after it the primary is used again.
    assert_eq!(router.next_delivery(&config, t(1_000)).unwrap().path, 1);
    assert_eq!(router.next_delivery(&config, t(5_000)).unwrap().path, 0);
}

#[test]
fn an_authorization_error_fails_over_and_the_path_waits_long() {
    let config = with_fallback();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"p");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, status(401), t(0));
    assert_eq!(router.queued(), 1, "the packet is kept");
    let d = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(d.path, 1);
    router.complete(d, Err(TransportError::Io), t(0));
    assert!(router.next_delivery(&config, t(4_000)).is_none());
    assert_eq!(router.next_delivery(&config, t(5_000)).unwrap().path, 1);
    assert_ne!(
        router.next_delivery(&config, t(599_999)).map(|d| d.path),
        Some(0)
    );
    assert_eq!(router.next_delivery(&config, t(600_000)).unwrap().path, 0);
}

#[test]
fn a_permanent_rejection_drops_the_packet_without_trying_the_fallback() {
    let config = with_fallback();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"p");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, status(400), t(0));
    assert_eq!(router.queued(), 0);
    assert!(router.next_delivery(&config, t(0)).is_none());
    assert_eq!(view(&router, &config, 0, 0).counters.rejected, 1);
    assert_eq!(router.pool().in_use(), 0);
    let d = {
        let handle = packet(&mut router, 1, b"q");
        router.admit(0, handle, t(0));
        router.pool_mut().release(handle);
        router.next_delivery(&config, t(0)).unwrap()
    };
    router.complete(d, status(413), t(0));
    assert_eq!(view(&router, &config, 0, 0).counters.rejected, 2);
}

fn two_routes() -> RouteSet {
    let mut config = config();
    config
        .set_endpoint(1, 0, Some("http://b.example"), &TestPolicy)
        .unwrap();
    config
}

#[test]
fn routes_share_one_packet_and_stay_independent() {
    let config = two_routes();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"shared");
    assert_eq!(router.admit(0, handle, t(0)), Admit::Queued);
    assert_eq!(router.admit(1, handle, t(0)), Admit::Queued);
    router.pool_mut().release(handle);
    assert_eq!(router.pool().in_use(), 1, "one slot serves both routes");

    let first = router.next_delivery(&config, t(0)).unwrap();
    router.complete(first, Err(TransportError::Timeout), t(0));
    // Slot 0 is cooling down, slot 1 still delivers.
    let second = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(second.route, 1);
    router.complete(second, ok(), t(0));
    assert_eq!(router.pool().in_use(), 1, "slot 0 still holds the packet");
    let again = router.next_delivery(&config, t(5_000)).unwrap();
    router.complete(again, ok(), t(5_000));
    assert_eq!(router.pool().in_use(), 0);
}

#[test]
fn a_full_outbox_skips_the_new_packet_and_never_evicts_an_older_one() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let a = packet(&mut router, 1, b"a");
    let b = packet(&mut router, 1, b"b");
    let c = packet(&mut router, 1, b"c");
    assert_eq!(router.admit(0, a, t(0)), Admit::Queued);
    assert_eq!(router.admit(0, b, t(0)), Admit::Queued);
    assert_eq!(router.admit(0, c, t(0)), Admit::OutboxFull);
    for handle in [a, b, c] {
        router.pool_mut().release(handle);
    }
    assert_eq!(view(&router, &config, 0, 0).counters.skipped_full, 1);
    let head = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(router.pool().get(head.packet).unwrap().1, b"a");
    assert_eq!(router.pool().in_use(), 2);
}

#[test]
fn an_exhausted_pool_refuses_a_new_packet_and_frees_on_delivery() {
    let config = two_routes();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let mut handles = std::vec::Vec::new();
    while let Some((handle, buffer)) = router.pool_mut().alloc(stamp(1)) {
        buffer[0] = 1;
        assert!(router.pool_mut().seal(handle, 1));
        handles.push(handle);
    }
    assert_eq!(handles.len(), 4);
    assert!(router.pool_mut().alloc(stamp(1)).is_none());
    router.admit(0, handles[0], t(0));
    for handle in handles {
        router.pool_mut().release(handle);
    }
    assert_eq!(router.pool().in_use(), 1);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, ok(), t(0));
    assert_eq!(router.pool().in_use(), 0);
    assert!(router.pool_mut().alloc(stamp(1)).is_some());
}

#[test]
fn a_destination_edit_flushes_only_that_route_and_an_interval_edit_does_not() {
    let mut config = two_routes();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.admit(1, handle, t(0));
    router.pool_mut().release(handle);

    config.set_interval(1, 30).unwrap();
    router.sync(&config, exists, t(1));
    assert_eq!(router.queued(), 2);

    config
        .set_fallback(0, Some("http://f.example"), &TestPolicy)
        .unwrap();
    router.sync(&config, exists, t(2));
    let views = router.views(&config, t(2));
    let (zero, one) = (views[0].unwrap(), views[1].unwrap());
    assert_eq!(zero.queue_depth, 0);
    assert_eq!(zero.counters.invalidated, 1);
    assert_eq!(one.queue_depth, 1);
    assert_eq!(router.pool().in_use(), 1);
}

#[test]
fn deleting_a_route_frees_its_state_and_never_moves_the_other_slot() {
    let mut config = two_routes();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.admit(1, handle, t(0));
    router.pool_mut().release(handle);
    let before = view(&router, &config, 1, 0);

    config.set_endpoint(0, 0, None, &TestPolicy).unwrap();
    router.sync(&config, exists, t(1));
    let views = router.views(&config, t(1));
    assert!(views[0].is_none(), "the slot is empty, not shifted");
    assert_eq!(views[1], Some(before));
    assert_eq!(router.queued(), 1);
    assert_eq!(router.pool().in_use(), 1);
    assert_eq!(
        router.admit(0, handle, t(1)),
        Admit::OutboxFull,
        "no route in the slot"
    );
}

#[test]
fn removing_or_recreating_a_route_inherits_nothing() {
    let mut config = config();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Timeout), t(0));

    config.set_endpoint(0, 0, None, &TestPolicy).unwrap();
    config
        .set_endpoint(0, 0, Some("http://a.example"), &TestPolicy)
        .unwrap();
    router.sync(&config, exists, t(1_000));
    assert_eq!(router.queued(), 0);
    assert_eq!(router.pool().in_use(), 0);
    // A fresh route: due at once, no cooldown, no counters.
    assert_eq!(router.due(t(1_000)).as_slice(), [0]);
    assert_eq!(
        view(&router, &config, 0, 1_000).counters,
        RouteCounters::default()
    );
    // The result of a send of the earlier route is ignored.
    router.complete(d, ok(), t(1_001));
    assert_eq!(view(&router, &config, 0, 1_001).counters.delivered, 0);
}

#[test]
fn replacing_the_endpoint_of_a_slot_drops_the_old_queue_without_a_slot_change() {
    let mut config = config();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Timeout), t(0));

    config
        .set_endpoint(0, 0, Some("http://b.example"), &TestPolicy)
        .unwrap();
    router.sync(&config, exists, t(1));
    assert_eq!(router.queued(), 0);
    assert_eq!(router.pool().in_use(), 0);
    let view = view(&router, &config, 0, 1);
    assert_eq!(view.generation, config.get(0).unwrap().generation);
    assert_eq!(view.counters, RouteCounters::default());
    assert!(
        router.next_delivery(&config, t(1)).is_none(),
        "nothing stale"
    );
}

#[test]
fn a_profile_that_is_not_available_suspends_the_route_and_drops_its_queue() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);

    router.sync(&config, |_| false, t(10));
    let view = view(&router, &config, 0, 10);
    assert_eq!(view.state, RouteState::ProfileMissing);
    assert_eq!(view.queue_depth, 0);
    assert!(
        router.due(t(10_000_000)).is_empty(),
        "no publication, no rebinding"
    );
    // It is back with the profile.
    router.sync(&config, exists, t(20));
    assert_eq!(view_state(&router, &config), RouteState::Idle);
}

fn view_state(router: &R, config: &RouteSet) -> RouteState {
    view(router, config, 0, 20).state
}

#[test]
fn stale_policy_packets_are_invalidated_and_checked_before_a_send_starts() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    let old = packet(&mut router, 1, b"old");
    router.admit(0, old, t(0));
    router.pool_mut().release(old);
    let delivery = router.next_delivery(&config, t(0)).unwrap();
    assert!(router.head_is_current(&delivery, |s| s.revision == 1));
    assert!(!router.head_is_current(&delivery, |s| s.revision == 2));

    router.invalidate(|s| s.revision == 2);
    assert_eq!(router.queued(), 0);
    assert_eq!(router.pool().in_use(), 0);
    // A completion that arrives after the flush is ignored.
    router.complete(delivery, ok(), t(1));
    assert_eq!(view(&router, &config, 0, 1).counters.delivered, 0);
}

#[test]
fn the_wakeup_is_the_earliest_publication_or_cooldown() {
    let config = config();
    let mut router = R::new();
    assert_eq!(router.next_wakeup(&config), None);
    router.sync(&config, exists, t(0));
    assert_eq!(router.next_wakeup(&config), Some(t(0)));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Timeout), t(0));
    assert_eq!(router.next_wakeup(&config), Some(t(5_000)));
    assert_eq!(view(&router, &config, 0, 1).state, RouteState::Backoff);
}

#[test]
fn a_busy_route_cannot_starve_the_others() {
    let mut config = RouteSet::default();
    for (index, host) in ["http://a.example", "http://b.example", "http://c.example"]
        .into_iter()
        .enumerate()
    {
        config
            .set_endpoint(index, 0, Some(host), &TestPolicy)
            .unwrap();
    }
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    // Every route always has a packet waiting: slot 0 is refilled after
    // each of its deliveries, the others hold one packet each.
    let first = packet(&mut router, 1, b"x");
    for index in 0..3 {
        router.admit(index, first, t(0));
    }
    router.pool_mut().release(first);

    let mut served = [0u32; 3];
    for round in 0..9u64 {
        let delivery = router.next_delivery(&config, t(round)).unwrap();
        served[delivery.route] += 1;
        router.complete(delivery, ok(), t(round));
        let refill = packet(&mut router, 1, b"y");
        router.admit(0, refill, t(round));
        router.pool_mut().release(refill);
    }
    assert_eq!(served[1], 1, "{served:?}");
    assert_eq!(served[2], 1, "{served:?}");
    assert!(served[0] >= 6, "{served:?}");
}

fn queue(router: &mut R, bytes: &[u8], now: u64) {
    let handle = packet(router, 1, bytes);
    assert_eq!(router.admit(0, handle, t(now)), Admit::Queued);
    router.pool_mut().release(handle);
}

#[test]
fn an_incompatible_path_hands_this_packet_to_the_other_path_without_a_penalty() {
    let config = with_fallback();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    queue(&mut router, b"big", 0);

    let primary = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(primary.path, 0);
    router.complete(primary, Err(TransportError::Incompatible), t(0));
    // The fallback is offered at once; the primary is neither cooling nor counted as a retry.
    let fallback = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!((fallback.path, fallback.packet), (1, primary.packet));
    router.complete(fallback, ok(), t(0));
    assert_eq!(router.queued(), 0);
    let counters = view(&router, &config, 0, 0).counters;
    assert_eq!(
        (counters.delivered, counters.retries, counters.incompatible),
        (1, 0, 0)
    );

    // The next packet starts with every path open: nothing was disabled for good.
    queue(&mut router, b"small", 60_000);
    assert_eq!(router.next_delivery(&config, t(60_000)).unwrap().path, 0);
}

#[test]
fn a_packet_no_path_can_carry_is_dropped_once_and_counted() {
    let config = with_fallback();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    queue(&mut router, b"huge", 0);

    for path in 0..2 {
        let d = router.next_delivery(&config, t(0)).unwrap();
        assert_eq!(d.path, path);
        router.complete(d, Err(TransportError::Incompatible), t(0));
    }
    assert_eq!(
        router.queued(),
        0,
        "no retry loop on a packet nobody can carry"
    );
    assert!(router.next_delivery(&config, t(0)).is_none());
    assert_eq!(router.pool().in_use(), 0);
    let counters = view(&router, &config, 0, 0).counters;
    assert_eq!(
        (counters.incompatible, counters.rejected, counters.retries),
        (1, 0, 0)
    );
}

#[test]
fn an_incompatible_primary_and_a_failing_fallback_keep_the_packet_for_a_later_attempt() {
    let config = with_fallback();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    queue(&mut router, b"big", 0);

    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Incompatible), t(0));
    let d = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(d.path, 1);
    router.complete(d, Err(TransportError::Timeout), t(0));
    // Only the fallback cools down; the primary stays excluded for this packet.
    assert!(router.next_delivery(&config, t(4_999)).is_none());
    assert_eq!(router.next_wakeup(&config), Some(t(5_000)));
    assert_eq!(router.next_delivery(&config, t(5_000)).unwrap().path, 1);
    assert_eq!(router.queued(), 1);
}

#[test]
fn invalidating_another_packet_keeps_the_head_incompatibility() {
    let config = with_fallback();
    let mut router = R::new();
    router.sync(&config, exists, t(0));
    for (revision, bytes) in [(2, b"head".as_slice()), (1, b"stale".as_slice())] {
        let handle = packet(&mut router, revision, bytes);
        assert_eq!(router.admit(0, handle, t(0)), Admit::Queued);
        router.pool_mut().release(handle);
    }

    let primary = router.next_delivery(&config, t(0)).unwrap();
    router.complete(primary, Err(TransportError::Incompatible), t(0));
    let fallback = router.next_delivery(&config, t(0)).unwrap();
    router.complete(fallback, Err(TransportError::Timeout), t(0));

    router.invalidate(|stamp| stamp.revision == 2);
    assert_eq!(router.queued(), 1);
    assert_eq!(router.next_delivery(&config, t(5_000)).unwrap().path, 1);
}
