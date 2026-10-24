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
use std::collections::VecDeque;
use std::vec::Vec;

use embassy_futures::poll_once;

use super::*;

type Service<'a, T, const DEPTH: usize = 4> = ConnectivityService<
    'a,
    NoopRawMutex,
    &'a FakeSource,
    &'a FakeClock,
    FakeUtc,
    MockSigner,
    CountingNonce,
    T,
    NoDelay,
    DEPTH,
>;

const NOW: u64 = 10_000;
const UTC: u64 = 1_700_000_000_000;

struct FakeUtc(Cell<Option<u64>>);

impl UtcClock for FakeUtc {
    fn now_unix_ms(&self) -> Result<u64, ClockError> {
        self.0.get().ok_or(ClockError)
    }
}

struct FakeSource {
    snapshot: Cell<Option<MeasurementsSnapshot>>,
    calls: Cell<u32>,
}

impl SnapshotSource for &FakeSource {
    async fn snapshot(&self) -> Option<MeasurementsSnapshot> {
        self.calls.set(self.calls.get() + 1);
        self.snapshot.get()
    }
}

/// Answers from a script and records the exact bytes of every attempt.
struct Scripted<'a> {
    script: VecDeque<Result<TransportAck, TransportError>>,
    sent: &'a RefCell<Vec<Vec<u8>>>,
}

impl EnvelopeTransport for Scripted<'_> {
    async fn send(&mut self, frame: &EnvelopeFrame) -> Result<TransportAck, TransportError> {
        self.sent.borrow_mut().push(frame.as_bytes().to_vec());
        self.script
            .pop_front()
            .unwrap_or(Err(TransportError::Connect))
    }
}

fn accepted() -> Result<TransportAck, TransportError> {
    Ok(TransportAck { status: 202 })
}

struct Fixture {
    clock: FakeClock,
    source: FakeSource,
    sent: RefCell<Vec<Vec<u8>>>,
    shared: Shared,
}

impl Fixture {
    fn new() -> Self {
        Self {
            clock: FakeClock(Cell::new(NOW)),
            source: FakeSource {
                // Generated 1 s before `NOW`.
                snapshot: Cell::new(Some(snapshot_of(
                    &[bme280(NOW - 1_500), sds011(NOW - 1_500)],
                    NOW - 1_000,
                ))),
                calls: Cell::new(0),
            },
            sent: RefCell::new(Vec::new()),
            shared: Shared::new(),
        }
    }

    fn service<'a, const DEPTH: usize>(
        &'a self,
        utc: Option<u64>,
        signer: MockSigner,
        nonce: CountingNonce,
        script: impl IntoIterator<Item = Result<TransportAck, TransportError>>,
    ) -> Service<'a, Scripted<'a>, DEPTH> {
        let utc: &'a FakeUtc = std::boxed::Box::leak(std::boxed::Box::new(FakeUtc(Cell::new(utc))));
        let signer: &'a MockSigner = std::boxed::Box::leak(std::boxed::Box::new(signer));
        ConnectivityService::new(
            ServiceConfig::new(42, 10_000),
            &self.shared,
            &self.source,
            &self.clock,
            utc,
            signer,
            nonce,
            Scripted {
                script: script.into_iter().collect(),
                sent: &self.sent,
            },
            NoDelay,
        )
    }

    fn ok_service<'a>(
        &'a self,
        script: impl IntoIterator<Item = Result<TransportAck, TransportError>>,
    ) -> Service<'a, Scripted<'a>> {
        self.service(
            Some(UTC),
            MockSigner::default(),
            CountingNonce::default(),
            script,
        )
    }
}

#[test]
fn produce_queues_a_frame_with_the_utc_of_the_measurement_set() {
    let fx = Fixture::new();
    let mut service = fx.ok_service([]);
    run(service.produce());

    assert_eq!(service.queued(), 1);
    assert_eq!(service.diagnostics().frames_created, 1);
    let envelope: SignedEnvelope = decode(service.front().unwrap().as_bytes()).unwrap();
    let message = message_of(&envelope.message);
    let meta = message.metadata().unwrap();
    assert_eq!(meta.node_id, 42);
    // UTC now minus the age of the snapshot (monotonic).
    assert_eq!(meta.timestamp, UTC - 1_000);
}

#[test]
fn without_utc_no_frame_is_created_and_the_snapshot_is_not_even_taken() {
    let fx = Fixture::new();
    let mut service = fx.service::<4>(None, MockSigner::default(), CountingNonce::default(), []);
    run(service.produce());
    assert_eq!(service.queued(), 0);
    assert_eq!(service.diagnostics().skipped_no_utc, 1);
    assert_eq!(service.state(), ConnectivityState::WaitingForUtc);
    assert_eq!(fx.source.calls.get(), 0);
}

#[test]
fn a_clock_behind_the_snapshot_age_is_not_a_timestamp() {
    let fx = Fixture::new();
    let mut service = fx.service::<4>(
        Some(500),
        MockSigner::default(),
        CountingNonce::default(),
        [],
    );
    run(service.produce());
    assert_eq!(service.queued(), 0);
    assert_eq!(service.diagnostics().skipped_no_utc, 1);
}

#[test]
fn without_identity_no_frame_is_created() {
    let fx = Fixture::new();
    let mut service = fx.service::<4>(
        Some(UTC),
        MockSigner::failing(IdentityError::NotProvisioned),
        CountingNonce::default(),
        [],
    );
    run(service.produce());
    assert_eq!(service.queued(), 0);
    assert_eq!(service.diagnostics().skipped_no_identity, 1);
    assert_eq!(service.state(), ConnectivityState::WaitingForIdentity);
}

#[test]
fn missing_snapshot_and_missing_measurements_are_counted() {
    let fx = Fixture::new();
    let mut service = fx.ok_service([]);
    fx.source.snapshot.set(None);
    run(service.produce());
    assert_eq!(service.diagnostics().skipped_no_snapshot, 1);
    fx.source.snapshot.set(Some(snapshot_of(&[], NOW)));
    run(service.produce());
    assert_eq!(service.diagnostics().skipped_no_measurements, 1);
    assert_eq!(service.queued(), 0);
}

#[test]
fn unsupported_measurements_are_counted_in_the_diagnostics() {
    let fx = Fixture::new();
    fx.source.snapshot.set(Some(snapshot_of(
        &[
            sds011(NOW - 100),
            update(MeasurementUpdate::Ags3871(Ags3871Update {
                co: Ppm(1.0),
                timestamp: ts(NOW - 100),
            })),
        ],
        NOW - 50,
    )));
    let mut service = fx.ok_service([]);
    run(service.produce());
    assert_eq!(service.diagnostics().measurements_unsupported, 1);
}

#[test]
fn nonce_is_requested_once_per_frame_not_per_attempt() {
    let fx = Fixture::new();
    let mut service = fx.ok_service([
        Err(TransportError::Connect),
        Err(TransportError::Timeout),
        accepted(),
    ]);
    run(service.produce());
    for _ in 0..3 {
        run(service.deliver());
    }
    let sent = fx.sent.borrow();
    assert_eq!(sent.len(), 3);
    assert!(sent.iter().all(|attempt| attempt == &sent[0]));
    let envelope: SignedEnvelope = decode(&sent[0]).unwrap();
    assert_eq!(envelope.nonce.as_slice(), &[1u8; 16]);
    assert_eq!(service.queued(), 0);
    assert_eq!(service.diagnostics().frames_delivered, 1);
    assert_eq!(service.diagnostics().send_retries, 2);
}

#[test]
fn a_retry_is_byte_identical_even_when_time_and_snapshot_change() {
    let fx = Fixture::new();
    let mut service = fx.ok_service([Err(TransportError::Status(503)), accepted()]);
    run(service.produce());
    run(service.deliver());
    fx.clock.0.set(NOW + 600_000);
    fx.source
        .snapshot
        .set(Some(snapshot_of(&[microphone(NOW)], NOW + 600_000)));
    run(service.deliver());
    let sent = fx.sent.borrow();
    assert_eq!(sent[0], sent[1]);
}

#[test]
fn retryable_failures_back_off_exponentially_up_to_the_cap() {
    let fx = Fixture::new();
    let mut service = fx.ok_service([]);
    run(service.produce());
    let mut delays = Vec::new();
    for _ in 0..12 {
        let before = fx.clock.0.get();
        run(service.deliver());
        delays.push(service.retry_at_ms() - before);
        fx.clock.0.set(service.retry_at_ms());
    }
    assert_eq!(delays[..4], [2_000, 4_000, 8_000, 16_000]);
    assert!(delays.iter().all(|d| *d <= 300_000));
    assert_eq!(*delays.last().unwrap(), 300_000);
    assert_eq!(
        service.state(),
        ConnectivityState::Backoff(TransportError::Connect)
    );
    assert_eq!(service.queued(), 1);
}

#[test]
fn success_resets_the_backoff() {
    let fx = Fixture::new();
    let mut service = fx.ok_service([
        Err(TransportError::Connect),
        Err(TransportError::Connect),
        accepted(),
        Err(TransportError::Connect),
    ]);
    run(service.produce());
    run(service.produce());
    let mut delays = Vec::new();
    for _ in 0..4 {
        let before = fx.clock.0.get();
        run(service.deliver());
        delays.push(service.retry_at_ms().saturating_sub(before));
        fx.clock.0.set(fx.clock.0.get().max(service.retry_at_ms()));
    }
    // Two failures (2 s, 4 s), a success, then the delay starts over.
    assert_eq!(delays, [2_000, 4_000, 0, 2_000]);
}

#[test]
fn permanent_rejection_drops_the_frame_without_backoff() {
    let fx = Fixture::new();
    let mut service = fx.ok_service([Err(TransportError::Status(400)), accepted()]);
    run(service.produce());
    run(service.produce());
    run(service.deliver());
    assert_eq!(service.queued(), 1);
    assert_eq!(service.diagnostics().frames_rejected, 1);
    assert_eq!(service.retry_at_ms(), 0);
    run(service.deliver());
    assert_eq!(service.queued(), 0);
    assert_ne!(fx.sent.borrow()[0], fx.sent.borrow()[1]);
}

#[test]
fn authorization_failure_pauses_for_the_longest_backoff() {
    let fx = Fixture::new();
    let mut service = fx.ok_service([Err(TransportError::Status(403))]);
    run(service.produce());
    run(service.deliver());
    assert_eq!(service.queued(), 0);
    assert_eq!(service.retry_at_ms(), NOW + 300_000);
    assert_eq!(
        service.state(),
        ConnectivityState::Unauthorized(TransportError::Status(403))
    );
}

#[test]
fn a_full_outbox_skips_new_frames_without_touching_the_queued_ones() {
    let fx = Fixture::new();
    let mut service = fx.service::<2>(
        Some(UTC),
        MockSigner::default(),
        CountingNonce::default(),
        [],
    );
    for _ in 0..5 {
        run(service.produce());
    }
    assert_eq!(service.queued(), 2);
    assert_eq!(service.diagnostics().frames_created, 2);
    assert_eq!(service.diagnostics().skipped_outbox_full, 3);
    // No snapshot was taken for the skipped intervals.
    assert_eq!(fx.source.calls.get(), 2);
}

#[test]
fn the_status_is_published() {
    let fx = Fixture::new();
    let mut service = fx.ok_service([accepted()]);
    run(service.produce());
    run(service.deliver());
    let status = fx.shared.status();
    assert_eq!(status.state, ConnectivityState::Idle);
    assert_eq!(status.diagnostics.frames_delivered, 1);
    assert_eq!(status.outbox_len, 0);
}

/// Delay that advances the fake clock and records every wait; pends forever
/// after `limit` waits so that the run loop can be inspected.
struct ClockDelay<'a> {
    clock: &'a FakeClock,
    waits: &'a RefCell<Vec<u32>>,
    limit: usize,
}

impl DelayNs for ClockDelay<'_> {
    async fn delay_ns(&mut self, ns: u32) {
        self.delay_ms(ns / 1_000_000).await
    }

    async fn delay_ms(&mut self, ms: u32) {
        if self.waits.borrow().len() >= self.limit {
            core::future::pending::<()>().await;
        }
        self.waits.borrow_mut().push(ms);
        self.clock.0.set(self.clock.0.get() + u64::from(ms));
    }
}

#[test]
fn the_run_loop_never_spins_and_keeps_producing_while_the_uplink_is_down() {
    let fx = Fixture::new();
    let waits = RefCell::new(Vec::new());
    let utc = FakeUtc(Cell::new(Some(UTC)));
    let signer = MockSigner::default();
    let mut service: ConnectivityService<
        '_,
        NoopRawMutex,
        &FakeSource,
        &FakeClock,
        FakeUtc,
        MockSigner,
        CountingNonce,
        Scripted<'_>,
        ClockDelay<'_>,
        4,
    > = ConnectivityService::new(
        ServiceConfig::new(1, 10_000),
        &fx.shared,
        &fx.source,
        &fx.clock,
        &utc,
        &signer,
        CountingNonce::default(),
        Scripted {
            script: VecDeque::new(),
            sent: &fx.sent,
        },
        ClockDelay {
            clock: &fx.clock,
            waits: &waits,
            limit: 40,
        },
    );
    {
        let run = service.run();
        let mut run = core::pin::pin!(run);
        assert!(poll_once(run.as_mut()).is_pending());
    }
    let waits = waits.borrow();
    assert_eq!(waits.len(), 40);
    assert!(waits.iter().all(|wait| *wait >= 1), "no zero-length wait");
    // The uplink never worked, yet frames kept being produced until the
    // bounded outbox filled up, and the same bytes were retried each time.
    assert_eq!(service.queued(), 4);
    let sent = fx.sent.borrow();
    assert!(sent.len() > 4);
    let first = &sent[0];
    assert!(sent.iter().filter(|attempt| *attempt == first).count() > 1);
    assert!(service.diagnostics().skipped_outbox_full > 0);
    // Backoff bounded: far fewer attempts than waits-at-1ms would imply.
    assert!(sent.len() < 40);
}
