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

use core::time::Duration;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_hal::MonotonicTimestamp;

use crate::testutil::{ClockDelay, FakeSource, ManualClock};
use crate::{
    SyncFailure, SyncSchedule, Synchronizer, TimeConfig, TimeSample, TimeService, TimeSourceKind,
    TimeSyncState, UnixTime, sync_once,
};

/// 2026-01-01T00:00:00Z.
const FLOOR: UnixTime = UnixTime::from_secs(1_767_225_600);

type Service = TimeService<ManualClock, NoopRawMutex>;

fn service() -> (Service, ManualClock) {
    let clock = ManualClock::default();
    (
        TimeService::new(clock.clone(), TimeConfig::new(FLOOR)),
        clock,
    )
}

fn sample(secs: u64) -> TimeSample {
    TimeSample {
        unix_time: UnixTime::from_secs(secs),
        uncertainty: Some(Duration::from_millis(20)),
    }
}

const NOW: u64 = 1_780_000_000;

#[test]
fn starts_unsynchronized_and_never_invents_time() {
    let (service, clock) = service();
    // Uptime must not leak out as a timestamp.
    clock.advance_secs(5_000);
    assert_eq!(service.now(), None);
    let snapshot = service.snapshot();
    assert_eq!(snapshot.wall_time, None);
    assert_eq!(snapshot.state, TimeSyncState::Unsynchronized);
    assert_eq!(
        snapshot.monotonic,
        MonotonicTimestamp::from_millis(5_000_000)
    );
    let status = service.status();
    assert_eq!(status.state, TimeSyncState::Unsynchronized);
    assert_eq!(status.source, None);
    assert_eq!(status.last_sync_age, None);
    assert_eq!(status.uncertainty, None);
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(status.last_failure, None);
}

#[test]
fn valid_sample_sets_the_anchor_and_monotonic_time_advances_it() {
    let (service, clock) = service();
    clock.advance_secs(100);
    service.submit(TimeSourceKind::Ntp, sample(NOW)).unwrap();
    assert_eq!(service.now(), Some(UnixTime::from_secs(NOW)));

    clock.advance_ms(2_500);
    assert_eq!(
        service.now(),
        Some(UnixTime::from_millis(NOW * 1_000 + 2_500))
    );

    let snapshot = service.snapshot();
    assert_eq!(snapshot.state, TimeSyncState::Synchronized);
    assert_eq!(snapshot.monotonic, MonotonicTimestamp::from_millis(102_500));
    assert_eq!(snapshot.wall_time, service.now());

    let status = service.status();
    assert_eq!(status.source, Some(TimeSourceKind::Ntp));
    assert_eq!(status.last_sync_age, Some(Duration::from_millis(2_500)));
    assert_eq!(status.uncertainty, Some(Duration::from_millis(20)));
}

#[test]
fn implausible_samples_are_rejected_and_keep_the_anchor() {
    let (service, clock) = service();
    // Rejected without an anchor: stays unsynchronized.
    assert_eq!(
        service.submit(TimeSourceKind::Ntp, sample(0)),
        Err(SyncFailure::Implausible)
    );
    assert_eq!(service.now(), None);

    service.submit(TimeSourceKind::Ntp, sample(NOW)).unwrap();
    clock.advance_secs(10);

    let too_old = sample(FLOOR.as_secs() - 1);
    let too_new = TimeSample {
        unix_time: service.config().not_after.saturating_add_millis(1_000),
        ..sample(NOW)
    };
    let too_vague = TimeSample {
        uncertainty: Some(Duration::from_secs(60)),
        ..sample(NOW + 5_000)
    };
    for bad in [too_old, too_new, too_vague] {
        assert_eq!(
            service.submit(TimeSourceKind::Ntp, bad),
            Err(SyncFailure::Implausible)
        );
    }
    assert_eq!(service.now(), Some(UnixTime::from_secs(NOW + 10)));
    let status = service.status();
    assert_eq!(status.consecutive_failures, 3);
    assert_eq!(status.last_failure, Some(SyncFailure::Implausible));
    assert_eq!(status.state, TimeSyncState::Synchronized);
}

#[test]
fn floor_and_ceiling_are_inclusive_and_unknown_uncertainty_is_accepted() {
    let (service, _clock) = service();
    let unknown = TimeSample {
        unix_time: FLOOR,
        uncertainty: None,
    };
    service.submit(TimeSourceKind::External, unknown).unwrap();
    assert_eq!(service.now(), Some(FLOOR));
    let ceiling = TimeSample {
        unix_time: service.config().not_after,
        uncertainty: None,
    };
    service.submit(TimeSourceKind::External, ceiling).unwrap();
}

#[test]
fn failures_do_not_remove_the_anchor_and_success_resets_them() {
    let (service, clock) = service();
    service.submit(TimeSourceKind::Ntp, sample(NOW)).unwrap();
    service.record_failure(SyncFailure::Timeout);
    service.record_failure(SyncFailure::Resolve);
    clock.advance_secs(1);
    assert_eq!(service.now(), Some(UnixTime::from_secs(NOW + 1)));
    let status = service.status();
    assert_eq!(status.consecutive_failures, 2);
    assert_eq!(status.last_failure, Some(SyncFailure::Resolve));

    service
        .submit(TimeSourceKind::Ntp, sample(NOW + 1))
        .unwrap();
    let status = service.status();
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(status.last_failure, None);
}

#[test]
fn becomes_stale_but_keeps_serving_time_until_resynchronized() {
    let (service, clock) = service();
    service.submit(TimeSourceKind::Ntp, sample(NOW)).unwrap();
    let stale_after = service.config().stale_after;

    clock.advance_ms(stale_after.as_millis() as u64);
    assert_eq!(service.status().state, TimeSyncState::Synchronized);
    clock.advance_ms(1);
    assert_eq!(service.status().state, TimeSyncState::Stale);
    assert_eq!(service.snapshot().state, TimeSyncState::Stale);
    assert!(service.now().is_some());

    let fresh = NOW + stale_after.as_secs() + 1;
    service.submit(TimeSourceKind::Ntp, sample(fresh)).unwrap();
    assert_eq!(service.status().state, TimeSyncState::Synchronized);
}

#[test]
fn a_new_sample_steps_the_clock_forward_and_backward() {
    let (service, clock) = service();
    service.submit(TimeSourceKind::Ntp, sample(NOW)).unwrap();
    clock.advance_secs(60);

    // The anchor is replaced as is: no smoothing, no monotonic guarantee.
    service
        .submit(TimeSourceKind::Ntp, sample(NOW + 90))
        .unwrap();
    assert_eq!(service.now(), Some(UnixTime::from_secs(NOW + 90)));
    clock.advance_secs(10);
    service
        .submit(TimeSourceKind::Ntp, sample(NOW + 70))
        .unwrap();
    assert_eq!(service.now(), Some(UnixTime::from_secs(NOW + 70)));
}

#[test]
fn the_latest_valid_sample_of_any_source_wins() {
    let (service, _clock) = service();
    service.submit(TimeSourceKind::Rtc, sample(NOW)).unwrap();
    assert_eq!(service.status().source, Some(TimeSourceKind::Rtc));
    service
        .submit(TimeSourceKind::Ntp, sample(NOW + 1))
        .unwrap();
    assert_eq!(service.status().source, Some(TimeSourceKind::Ntp));
}

fn schedule() -> SyncSchedule {
    SyncSchedule {
        initial_retry_ms: 1_000,
        max_retry_ms: 4_000,
        resync_ms: 100_000,
        unavailable_poll_ms: 500,
    }
}

#[test]
fn sync_once_feeds_samples_and_failures_into_the_service() {
    let (service, _clock) = service();
    let mut source = FakeSource::new(TimeSourceKind::Rtc);
    source.script.push_back(Err(SyncFailure::Timeout));
    source.script.push_back(Ok(sample(NOW)));
    source.script.push_back(Ok(sample(1)));

    assert_eq!(
        block_on(sync_once(&service, &mut source)),
        Err(SyncFailure::Timeout)
    );
    assert_eq!(service.now(), None);

    assert_eq!(block_on(sync_once(&service, &mut source)), Ok(()));
    assert_eq!(service.status().source, Some(TimeSourceKind::Rtc));

    // A source returning garbage cannot replace the valid anchor.
    assert_eq!(
        block_on(sync_once(&service, &mut source)),
        Err(SyncFailure::Implausible)
    );
    assert_eq!(service.now(), Some(UnixTime::from_secs(NOW)));
}

#[test]
fn failures_back_off_exponentially_up_to_a_bound_and_success_resets() {
    let (service, clock) = service();
    let delay = ClockDelay::new(&clock);
    let mut source = FakeSource::new(TimeSourceKind::Ntp);
    for _ in 0..5 {
        source.script.push_back(Err(SyncFailure::Timeout));
    }
    source.script.push_back(Ok(sample(NOW)));
    source.script.push_back(Err(SyncFailure::Resolve));
    let mut sync = Synchronizer::new(&service, source, delay.clone(), schedule());

    for _ in 0..7 {
        block_on(sync.step());
    }
    assert_eq!(
        delay.waits(),
        [1_000, 2_000, 4_000, 4_000, 4_000, 100_000, 1_000]
    );
    let status = service.status();
    assert_eq!(status.consecutive_failures, 1);
    assert_eq!(status.last_failure, Some(SyncFailure::Resolve));
    assert_eq!(status.source, Some(TimeSourceKind::Ntp));
}

#[test]
fn unavailable_source_is_polled_without_counting_failures() {
    let (service, clock) = service();
    let delay = ClockDelay::new(&clock);
    let mut source = FakeSource::new(TimeSourceKind::Ntp);
    source.script.push_back(Ok(sample(NOW)));
    let available = source.available.clone();
    available.set(false);
    let mut sync = Synchronizer::new(&service, source, delay.clone(), schedule());

    block_on(sync.step());
    block_on(sync.step());
    assert_eq!(delay.waits(), [500, 500]);
    let status = service.status();
    assert_eq!(status.state, TimeSyncState::Unsynchronized);
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(status.last_failure, Some(SyncFailure::NetworkUnavailable));

    // The network comes back: the first sample is taken at once.
    available.set(true);
    block_on(sync.step());
    assert_eq!(service.status().state, TimeSyncState::Synchronized);
    assert_eq!(service.status().last_failure, None);
}

#[test]
fn network_loss_keeps_time_then_stale_then_resync_recovers() {
    let (service, clock) = service();
    let delay = ClockDelay::new(&clock);
    let mut source = FakeSource::new(TimeSourceKind::Ntp);
    source.script.push_back(Ok(sample(NOW)));
    let available = source.available.clone();
    let mut sync = Synchronizer::new(&service, source, delay.clone(), schedule());

    // Sync succeeds; the step also waits the resync interval.
    block_on(sync.step());
    assert_eq!(service.status().state, TimeSyncState::Synchronized);

    available.set(false);
    let stale_polls = service.config().stale_after.as_millis() as usize / 500 + 1;
    for _ in 0..stale_polls {
        block_on(sync.step());
    }
    // Wall time continued from the monotonic anchor, the state turned stale.
    let status = service.status();
    assert_eq!(status.state, TimeSyncState::Stale);
    assert!(service.now().unwrap() > UnixTime::from_secs(NOW + 21_000));

    // Network returns: resynchronization succeeds.
    available.set(true);
    sync.source_mut_for_test()
        .script
        .push_back(Ok(sample(NOW + 30_000)));
    block_on(sync.step());
    assert_eq!(service.status().state, TimeSyncState::Synchronized);
}

#[test]
fn consumers_are_independent_of_the_source_type() {
    // A consumer only sees the service, whatever feeds it.
    fn timestamp<M: embassy_sync::blocking_mutex::raw::RawMutex>(
        service: &TimeService<ManualClock, M>,
    ) -> Option<u64> {
        service.now().map(UnixTime::as_secs)
    }

    for kind in [
        TimeSourceKind::Ntp,
        TimeSourceKind::Rtc,
        TimeSourceKind::External,
    ] {
        let (service, _clock) = service();
        let mut source = FakeSource::new(kind);
        source.script.push_back(Ok(sample(NOW)));
        assert_eq!(timestamp(&service), None);
        block_on(sync_once(&service, &mut source)).unwrap();
        assert_eq!(timestamp(&service), Some(NOW));
        assert_eq!(service.status().source, Some(kind));
    }
}

#[test]
fn pivot_is_only_a_guess_before_the_first_sample() {
    let (service, clock) = service();
    clock.advance_secs(30);
    assert_eq!(service.pivot(), UnixTime::from_secs(FLOOR.as_secs() + 30));
    assert_eq!(service.now(), None);

    service.submit(TimeSourceKind::Ntp, sample(NOW)).unwrap();
    clock.advance_secs(1);
    assert_eq!(service.pivot(), UnixTime::from_secs(NOW + 1));
}
