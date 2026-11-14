use rohi_telemetry::PacketProfileId;

use super::*;
use crate::config::ProfileBinding;

type R = Router<2, 4, 16>;

fn t(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

fn binding(incarnation: u32) -> ProfileBinding {
    ProfileBinding {
        id: PacketProfileId::new("p").unwrap(),
        incarnation,
    }
}

fn stamp(revision: u32) -> ProfileStamp {
    ProfileStamp {
        id: PacketProfileId::new("p").unwrap(),
        incarnation: 1,
        revision,
    }
}

fn resolve(id: &str) -> Option<u32> {
    (id == "p").then_some(1)
}

fn config() -> RouteSet {
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(1), "http://a.example")
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
    Ok(TransportAck { status: 202 })
}

#[test]
fn a_new_route_is_due_at_once_then_every_interval_and_unrelated_edits_do_not_move_it() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, resolve, t(1_000));
    assert_eq!(router.due(t(1_000)).as_slice(), [0]);

    let handle = packet(&mut router, 1, b"x");
    assert_eq!(router.admit(0, handle, t(1_000)), Admit::Queued);
    router.pool_mut().release(handle);
    assert!(router.due(t(60_999)).is_empty());
    assert_eq!(router.due(t(61_000)).as_slice(), [0]);
    // A sync with an unchanged configuration keeps the deadline.
    router.sync(&config, resolve, t(30_000));
    // The queued packet is deliverable now: that is the earliest wakeup.
    assert_eq!(router.next_wakeup(&config), Some(t(0)));
    let d = router.next_delivery(&config, t(1_000)).unwrap();
    router.complete(d, ok(), t(1_000));
    assert_eq!(router.next_wakeup(&config), Some(t(61_000)));
}

#[test]
fn an_interval_edit_rebases_the_deadline_without_clearing_the_queue_or_publishing_extra() {
    let mut config = config();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);

    config.set_interval("a", 120).unwrap();
    router.sync(&config, resolve, t(10_000));
    assert!(router.due(t(10_000)).is_empty(), "no extra frame");
    assert!(router.due(t(129_999)).is_empty());
    assert_eq!(router.due(t(130_000)).as_slice(), [0]);
    assert_eq!(router.queued(), 1, "the queue is kept");
}

#[test]
fn only_202_completes_a_packet_and_the_same_bytes_are_retried() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
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
    router.complete(again, crate::classify_status(200), t(5_000));
    assert_eq!(router.queued(), 1);
    let third = router.next_delivery(&config, t(15_000)).unwrap();
    router.complete(third, ok(), t(15_000));
    assert_eq!(router.queued(), 0);
    assert_eq!(router.pool().in_use(), 0, "every reference was released");
    let view = &router.views(&config, t(15_000))[0];
    assert_eq!((view.counters.delivered, view.counters.retries), (1, 2));
}

fn with_fallbacks() -> RouteSet {
    let mut config = config();
    config
        .add_fallback("a", "one", "http://one.example")
        .unwrap();
    config
        .add_fallback("a", "two", "http://two.example")
        .unwrap();
    config
}

#[test]
fn a_failing_primary_fails_over_at_once_and_a_fallback_ack_completes_the_route() {
    let config = with_fallbacks();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
    let handle = packet(&mut router, 1, b"p");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);

    let d = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(d.path, 0);
    router.complete(d, Err(TransportError::Status(503)), t(0));
    let d = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(
        d.path, 1,
        "the first fallback, without waiting for the primary"
    );
    router.complete(d, Err(TransportError::Dns), t(0));
    let d = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(d.path, 2);
    router.complete(d, ok(), t(0));

    assert_eq!(router.queued(), 0);
    let view = &router.views(&config, t(0))[0];
    assert_eq!((view.counters.delivered, view.counters.failovers), (1, 1));
}

#[test]
fn the_primary_is_preferred_again_when_its_cooldown_ends() {
    let config = with_fallbacks();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
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
    let config = with_fallbacks();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
    let handle = packet(&mut router, 1, b"p");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Status(401)), t(0));
    assert_eq!(router.queued(), 1, "the packet is kept");
    assert_eq!(router.next_delivery(&config, t(0)).unwrap().path, 1);
    // Fail every path: the next attempt of the primary is 10 minutes away.
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Io), t(0));
    let d = router.next_delivery(&config, t(0)).unwrap();
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
fn a_permanent_rejection_drops_the_packet_without_trying_other_endpoints() {
    let config = with_fallbacks();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
    let handle = packet(&mut router, 1, b"p");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Status(400)), t(0));
    assert_eq!(router.queued(), 0);
    assert!(router.next_delivery(&config, t(0)).is_none());
    assert_eq!(router.views(&config, t(0))[0].counters.rejected, 1);
    assert_eq!(router.pool().in_use(), 0);
    let d = {
        let handle = packet(&mut router, 1, b"q");
        router.admit(0, handle, t(0));
        router.pool_mut().release(handle);
        router.next_delivery(&config, t(0)).unwrap()
    };
    router.complete(d, Err(TransportError::Status(413)), t(0));
    assert_eq!(router.views(&config, t(0))[0].counters.rejected, 2);
}

#[test]
fn routes_share_one_packet_and_stay_independent() {
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(1), "http://a.example")
        .unwrap();
    config
        .add_route("b", binding(1), "http://b.example")
        .unwrap();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
    let handle = packet(&mut router, 1, b"shared");
    assert_eq!(router.admit(0, handle, t(0)), Admit::Queued);
    assert_eq!(router.admit(1, handle, t(0)), Admit::Queued);
    router.pool_mut().release(handle);
    assert_eq!(router.pool().in_use(), 1, "one slot serves both routes");

    let first = router.next_delivery(&config, t(0)).unwrap();
    router.complete(first, Err(TransportError::Timeout), t(0));
    // Route a is cooling down, route b still delivers.
    let second = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(second.route, 1);
    router.complete(second, ok(), t(0));
    assert_eq!(router.pool().in_use(), 1, "route a still holds the packet");
    let again = router.next_delivery(&config, t(5_000)).unwrap();
    router.complete(again, ok(), t(5_000));
    assert_eq!(router.pool().in_use(), 0);
}

#[test]
fn a_full_outbox_skips_the_new_packet_and_never_evicts_an_older_one() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
    let a = packet(&mut router, 1, b"a");
    let b = packet(&mut router, 1, b"b");
    let c = packet(&mut router, 1, b"c");
    assert_eq!(router.admit(0, a, t(0)), Admit::Queued);
    assert_eq!(router.admit(0, b, t(0)), Admit::Queued);
    assert_eq!(router.admit(0, c, t(0)), Admit::OutboxFull);
    for handle in [a, b, c] {
        router.pool_mut().release(handle);
    }
    assert_eq!(router.views(&config, t(0))[0].counters.skipped_full, 1);
    let head = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(router.pool().get(head.packet).unwrap().1, b"a");
    assert_eq!(router.pool().in_use(), 2);
}

#[test]
fn a_destination_edit_flushes_only_that_route_and_an_interval_edit_does_not() {
    let mut config = RouteSet::default();
    config
        .add_route("a", binding(1), "http://a.example")
        .unwrap();
    config
        .add_route("b", binding(1), "http://b.example")
        .unwrap();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.admit(1, handle, t(0));
    router.pool_mut().release(handle);

    config.set_interval("b", 30).unwrap();
    router.sync(&config, resolve, t(1));
    assert_eq!(router.queued(), 2);

    config.add_fallback("a", "f", "http://f.example").unwrap();
    router.sync(&config, resolve, t(2));
    let views = router.views(&config, t(2));
    assert_eq!(views[0].queue_depth, 0);
    assert_eq!(views[0].counters.invalidated, 1);
    assert_eq!(views[1].queue_depth, 1);
    assert_eq!(router.pool().in_use(), 1);
}

#[test]
fn removing_or_recreating_a_route_inherits_nothing() {
    let mut config = config();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Timeout), t(0));

    config.remove_route("a").unwrap();
    config
        .add_route("a", binding(1), "http://a.example")
        .unwrap();
    router.sync(&config, resolve, t(1_000));
    assert_eq!(router.queued(), 0);
    assert_eq!(router.pool().in_use(), 0);
    // A fresh route: due at once, no cooldown, no counters.
    assert_eq!(router.due(t(1_000)).as_slice(), [0]);
    assert_eq!(
        router.views(&config, t(1_000))[0].counters,
        RouteCounters::default()
    );
}

#[test]
fn a_profile_that_changed_incarnation_suspends_the_route_and_drops_its_queue() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);

    // The profile was deleted and recreated: another incarnation.
    router.sync(&config, |_| Some(7), t(10));
    let view = &router.views(&config, t(10))[0];
    assert_eq!(view.state, RouteState::ProfileMissing);
    assert_eq!(view.queue_depth, 0);
    assert!(
        router.due(t(10_000_000)).is_empty(),
        "no publication, no rebinding"
    );
    // The profile is gone entirely: the same.
    router.sync(&config, |_| None, t(20));
    assert!(router.due(t(10_000_000)).is_empty());
}

#[test]
fn stale_policy_packets_are_invalidated_and_checked_before_a_send_starts() {
    let config = config();
    let mut router = R::new();
    router.sync(&config, resolve, t(0));
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
    assert_eq!(router.views(&config, t(1))[0].counters.delivered, 0);
}

#[test]
fn the_wakeup_is_the_earliest_publication_or_cooldown() {
    let config = config();
    let mut router = R::new();
    assert_eq!(router.next_wakeup(&config), None);
    router.sync(&config, resolve, t(0));
    assert_eq!(router.next_wakeup(&config), Some(t(0)));
    let handle = packet(&mut router, 1, b"x");
    router.admit(0, handle, t(0));
    router.pool_mut().release(handle);
    let d = router.next_delivery(&config, t(0)).unwrap();
    router.complete(d, Err(TransportError::Timeout), t(0));
    assert_eq!(router.next_wakeup(&config), Some(t(5_000)));
    assert_eq!(
        router.views(&config, t(1)).first().unwrap().state,
        RouteState::Backoff
    );
}
