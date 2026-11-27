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
use core::future::Future;
use core::task::Poll;
use std::collections::VecDeque;
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_futures::yield_now;
use rohi_connectivity::Disposition;

use super::*;
use crate::frame::{CONTROL_FRAGMENT, FRAGMENT_BODY_MAX, MAX_PAYLOAD};

const GATEWAY: &str = "meshtastic+uart://!deadbeef";

/// What the radio was asked to send.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    destination: u32,
    id: u32,
    port: u32,
    payload: Vec<u8>,
    want_ack: bool,
    pki: bool,
}

#[derive(Default)]
struct Script {
    /// What the radio reports, in this order.
    results: VecDeque<Result<RoutingResult, RadioError>>,
    /// Acknowledge everything sent so far, last packet first.
    ack_all: bool,
}

struct FakeRadio<'a> {
    sent: &'a RefCell<Vec<Sent>>,
    script: &'a RefCell<Script>,
    next: u32,
    fail_send_at: Option<usize>,
}

impl MeshtasticRadio for FakeRadio<'_> {
    fn reserve_ids(&mut self, count: usize) -> u32 {
        let first = self.next;
        self.next += count as u32;
        first
    }

    async fn send(&mut self, packet: &Outgoing<'_>) -> Result<(), RadioError> {
        if self.fail_send_at == Some(self.sent.borrow().len()) {
            return Err(RadioError::NotConnected);
        }
        self.sent.borrow_mut().push(Sent {
            destination: packet.destination,
            id: packet.id,
            port: packet.port,
            payload: packet.payload.to_vec(),
            want_ack: packet.want_ack,
            pki: packet.pki,
        });
        Ok(())
    }

    async fn next_result(&mut self) -> Result<RoutingResult, RadioError> {
        loop {
            {
                let mut script = self.script.borrow_mut();
                if script.ack_all {
                    script.ack_all = false;
                    for packet in self.sent.borrow().iter().rev() {
                        script.results.push_back(ack(packet.id));
                    }
                }
                if let Some(result) = script.results.pop_front() {
                    return result;
                }
            }
            yield_now().await;
        }
    }
}

fn ack(id: u32) -> Result<RoutingResult, RadioError> {
    Ok(RoutingResult {
        request_id: id,
        error: 0,
    })
}

/// A timeout that never fires.
struct NeverDelay;

impl embedded_hal_async::delay::DelayNs for NeverDelay {
    async fn delay_ns(&mut self, _ns: u32) {
        core::future::pending::<()>().await
    }
}

/// A timeout that fires once the attempt has had time to do everything it can.
struct IdleDelay;

impl embedded_hal_async::delay::DelayNs for IdleDelay {
    async fn delay_ns(&mut self, _ns: u32) {
        for _ in 0..50 {
            yield_now().await;
        }
    }
}

struct Fixture {
    sent: RefCell<Vec<Sent>>,
    script: RefCell<Script>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            sent: RefCell::new(Vec::new()),
            script: RefCell::new(Script::default()),
        }
    }

    fn radio(&self) -> FakeRadio<'_> {
        FakeRadio {
            sent: &self.sent,
            script: &self.script,
            next: 100,
            fail_send_at: None,
        }
    }

    fn transport(&self) -> MeshtasticTransport<FakeRadio<'_>, IdleDelay> {
        MeshtasticTransport::new(Link::Uart, self.radio(), IdleDelay)
    }

    fn ack_all(&self) {
        self.script.borrow_mut().ack_all = true;
    }

    fn script(&self, results: impl IntoIterator<Item = Result<RoutingResult, RadioError>>) {
        self.script.borrow_mut().results = results.into_iter().collect();
    }
}

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 13 + 5) as u8).collect()
}

fn deliver<D: embedded_hal_async::delay::DelayNs>(
    transport: &mut MeshtasticTransport<FakeRadio<'_>, D>,
    uri: &str,
    packet: &[u8],
) -> Result<TransportAck, TransportError> {
    block_on(transport.deliver(uri, packet))
}

#[test]
fn a_single_frame_is_a_pki_unicast_with_an_ack_request_on_the_private_port() {
    let f = Fixture::new();
    f.ack_all();
    let mut transport = f.transport();
    let packet = payload(218);
    let result = deliver(&mut transport, GATEWAY, &packet).unwrap();
    assert_eq!((result.kind, result.detail), (AckKind::Relayed, 1));
    let sent = f.sent.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].destination, 0xdead_beef,
        "the gateway, never a broadcast"
    );
    assert_eq!(sent[0].port, 256);
    assert!(
        sent[0].want_ack && sent[0].pki,
        "PKI unicast with a routing ack, no channel fallback"
    );
    assert_eq!(sent[0].payload[0], 0x01);
    assert_eq!(
        sent[0].payload[1..],
        packet[..],
        "the sealed bytes, unchanged"
    );
}

#[test]
fn a_large_packet_is_fragmented_in_order_with_distinct_ids_and_needs_every_ack() {
    let f = Fixture::new();
    f.ack_all();
    let mut transport = f.transport();
    let packet = payload(1_000);
    let result = deliver(&mut transport, GATEWAY, &packet).unwrap();
    assert_eq!((result.kind, result.detail), (AckKind::Relayed, 5));
    let sent = f.sent.borrow();
    assert_eq!(
        sent.iter().map(|s| s.id).collect::<Vec<_>>(),
        [100, 101, 102, 103, 104]
    );
    let mut rebuilt = Vec::new();
    for (index, frame) in sent.iter().enumerate() {
        assert_eq!(
            (frame.destination, frame.port, frame.want_ack, frame.pki),
            (0xdead_beef, 256, true, true)
        );
        assert_eq!(frame.payload[0], CONTROL_FRAGMENT);
        assert_eq!(
            usize::from(frame.payload[7] & 0x0f),
            index,
            "ascending order"
        );
        rebuilt.extend_from_slice(&frame.payload[8..]);
    }
    assert_eq!(rebuilt, packet);
}

#[test]
fn acks_are_matched_by_id_in_any_order_and_unrelated_late_or_duplicate_ones_are_ignored() {
    let f = Fixture::new();
    f.script([
        ack(9_999), // unrelated
        ack(102),   // out of order
        ack(100),
        ack(100), // duplicate
        ack(5),   // late result of an earlier attempt
        ack(101),
    ]);
    let mut transport = f.transport();
    assert_eq!(
        deliver(&mut transport, GATEWAY, &payload(500))
            .unwrap()
            .detail,
        3
    );
}

#[test]
fn some_acks_are_not_enough() {
    let f = Fixture::new();
    f.script([ack(100), ack(101)]);
    let mut transport = f.transport();
    // Three fragments, two acknowledged: the attempt times out instead of succeeding.
    assert_eq!(
        deliver(&mut transport, GATEWAY, &payload(500)),
        Err(TransportError::Timeout)
    );
    assert_eq!(f.sent.borrow().len(), 3);
}

#[test]
fn a_routing_failure_ends_the_attempt_and_is_classified() {
    for (error, expected) in [
        (5u8, Disposition::Transient),   // MAX_RETRANSMIT
        (9, Disposition::Transient),     // DUTY_CYCLE_LIMIT
        (3, Disposition::Transient),     // TIMEOUT
        (34, Disposition::Unauthorized), // PKI_FAILED
        (35, Disposition::Unauthorized), // PKI_UNKNOWN_PUBKEY
        (33, Disposition::Unauthorized), // NOT_AUTHORIZED
        (7, Disposition::Incompatible),  // TOO_LARGE
    ] {
        let f = Fixture::new();
        f.script([
            ack(100),
            Ok(RoutingResult {
                request_id: 101,
                error,
            }),
        ]);
        let mut transport = f.transport();
        let result = deliver(&mut transport, GATEWAY, &payload(500));
        assert_eq!(result.unwrap_err().disposition(), expected, "error {error}");
    }
}

#[test]
fn a_failure_of_an_already_acknowledged_fragment_is_a_duplicate_and_ignored() {
    let f = Fixture::new();
    f.script([
        ack(100),
        Ok(RoutingResult {
            request_id: 100,
            error: 5,
        }),
        ack(101),
        ack(102),
    ]);
    let mut transport = f.transport();
    assert!(deliver(&mut transport, GATEWAY, &payload(500)).is_ok());
}

#[test]
fn a_dead_link_is_unavailable_and_a_lost_link_ends_the_wait() {
    // The radio refuses the second fragment.
    let f = Fixture::new();
    let radio = FakeRadio {
        fail_send_at: Some(1),
        ..f.radio()
    };
    let mut transport = MeshtasticTransport::new(Link::Uart, radio, IdleDelay);
    assert_eq!(
        deliver(&mut transport, GATEWAY, &payload(500)),
        Err(TransportError::LinkUnavailable)
    );
    assert_eq!(f.sent.borrow().len(), 1, "nothing after the failure");

    // The link goes down while waiting.
    let f = Fixture::new();
    f.script([ack(100), Err(RadioError::Disconnected)]);
    let mut transport = f.transport();
    assert_eq!(
        deliver(&mut transport, GATEWAY, &payload(500)),
        Err(TransportError::LinkUnavailable)
    );

    let f = Fixture::new();
    f.script([Err(RadioError::Io)]);
    let mut transport = f.transport();
    assert_eq!(
        deliver(&mut transport, GATEWAY, &payload(10)),
        Err(TransportError::Io)
    );
}

#[test]
fn a_packet_that_cannot_be_framed_is_incompatible_and_nothing_is_sent() {
    let f = Fixture::new();
    let mut transport = f.transport();
    for len in [0, MAX_PAYLOAD + 1, 3_500] {
        let result = deliver(&mut transport, GATEWAY, &payload(len));
        assert_eq!(result, Err(TransportError::Incompatible), "len {len}");
    }
    assert!(
        f.sent.borrow().is_empty(),
        "no partial sequence for a preflight failure"
    );
    // The limit itself is carried.
    f.ack_all();
    assert_eq!(
        deliver(&mut transport, GATEWAY, &payload(MAX_PAYLOAD))
            .unwrap()
            .detail,
        16
    );
    assert_eq!(
        f.sent
            .borrow()
            .iter()
            .map(|s| s.payload.len() - 8)
            .sum::<usize>(),
        MAX_PAYLOAD
    );
    assert_eq!(FRAGMENT_BODY_MAX * 16, MAX_PAYLOAD);
}

#[test]
fn only_the_uri_of_its_own_link_is_served() {
    let f = Fixture::new();
    let mut transport = f.transport();
    assert!(transport.handles("meshtastic+uart"));
    assert!(!transport.handles("meshtastic+ble") && !transport.handles("http"));
    for uri in [
        "meshtastic+ble://!deadbeef",
        "meshtastic+uart://!deadbeef?device=1",
        "meshtastic+uart://deadbeef",
        "http://a",
    ] {
        assert_eq!(
            deliver(&mut transport, uri, &payload(10)),
            Err(TransportError::Unsupported),
            "{uri}"
        );
    }
    assert!(f.sent.borrow().is_empty());
}

#[test]
fn ids_of_a_dropped_attempt_are_never_reused() {
    let f = Fixture::new();
    let mut transport = f.transport();
    // An attempt is dropped while it waits for acks (a route change).
    {
        let packet = payload(500);
        let attempt = transport.deliver(GATEWAY, &packet);
        let mut attempt = core::pin::pin!(attempt);
        let mut polls = 0;
        block_on(core::future::poll_fn(|cx| {
            polls += 1;
            let _ = attempt.as_mut().poll(cx);
            if polls < 5 {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }));
    }
    assert_eq!(
        f.sent.borrow().iter().map(|s| s.id).collect::<Vec<_>>(),
        [100, 101, 102]
    );
    f.sent.borrow_mut().clear();
    // Late acks of the dropped attempt do not complete the next one; its own does.
    f.script([ack(100), ack(101), ack(102)]);
    f.ack_all();
    assert!(deliver(&mut transport, GATEWAY, &payload(10)).is_ok());
    assert_eq!(f.sent.borrow()[0].id, 103, "a fresh id");
}

#[test]
fn a_retry_sends_the_same_bytes() {
    let f = Fixture::new();
    let mut transport = f.transport();
    let packet = payload(700);
    assert_eq!(
        deliver(&mut transport, GATEWAY, &packet),
        Err(TransportError::Timeout)
    );
    let first: Vec<Vec<u8>> = f.sent.borrow().iter().map(|s| s.payload.clone()).collect();
    f.sent.borrow_mut().clear();
    assert_eq!(
        deliver(&mut transport, GATEWAY, &packet),
        Err(TransportError::Timeout)
    );
    let second: Vec<Vec<u8>> = f.sent.borrow().iter().map(|s| s.payload.clone()).collect();
    assert_eq!(
        first, second,
        "the fragments are identical, including the message id"
    );
}

#[test]
fn a_silent_radio_times_out_and_an_answering_one_does_not_need_the_timer() {
    let f = Fixture::new();
    let mut transport = MeshtasticTransport::new(Link::Uart, f.radio(), NeverDelay);
    f.ack_all();
    assert!(deliver(&mut transport, GATEWAY, &payload(300)).is_ok());
    let silent = Fixture::new();
    let mut transport = silent.transport().with_ack_timeout_ms(10);
    assert_eq!(
        deliver(&mut transport, GATEWAY, &payload(300)),
        Err(TransportError::Timeout)
    );
}

/// Remembers how long it was asked to wait, and then waits for the radio.
struct RecordingDelay<'a>(&'a core::cell::Cell<u32>);

impl embedded_hal_async::delay::DelayNs for RecordingDelay<'_> {
    async fn delay_ns(&mut self, ns: u32) {
        // `delay_ms` arrives in chunks of at most u32::MAX ns.
        self.0.set(self.0.get() + ns / 1_000_000);
        IdleDelay.delay_ns(0).await;
    }
}

#[test]
fn one_attempt_is_bounded_whatever_the_number_of_fragments() {
    let f = Fixture::new();
    let waited = core::cell::Cell::new(0);
    let mut transport = MeshtasticTransport::new(Link::Uart, f.radio(), RecordingDelay(&waited));
    // 16 fragments at the default 15 s each would be 4 minutes.
    let _ = deliver(&mut transport, GATEWAY, &payload(MAX_PAYLOAD));
    assert_eq!(waited.get(), MAX_ATTEMPT_MS);
    // A small packet gets its fragments' share.
    waited.set(0);
    let _ = deliver(&mut transport, GATEWAY, &payload(10));
    assert_eq!(waited.get(), DEFAULT_ACK_TIMEOUT_MS);
}
