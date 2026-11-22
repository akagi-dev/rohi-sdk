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
use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use super::*;
use crate::proto::{FromRadio, decode_from_radio};

type Link = MeshtasticLink<NoopRawMutex>;

fn outgoing(id: u32) -> Outgoing<'static> {
    Outgoing {
        destination: 0xdead_beef,
        id,
        port: 256,
        payload: &[1, 2, 3],
        want_ack: true,
        pki: true,
    }
}

fn result(request_id: u32, error: u8) -> RoutingResult {
    RoutingResult { request_id, error }
}

#[test]
fn nothing_is_sent_to_a_radio_that_is_not_connected() {
    let link = Link::new();
    let mut radio = LinkRadio::new(&link);
    assert_eq!(
        block_on(radio.send(&outgoing(1))),
        Err(RadioError::NotConnected)
    );
    assert!(!link.snapshot().connected);
}

#[test]
fn a_packet_reaches_the_backend_as_an_encoded_to_radio() {
    let link = Link::new();
    link.session_up(0);
    let mut radio = LinkRadio::new(&link);
    block_on(radio.send(&outgoing(7))).unwrap();
    let message = block_on(link.next_outgoing());
    let mut expected = [0u8; 64];
    let len = encode_packet(&mut expected, &outgoing(7)).unwrap();
    assert_eq!(message.as_slice(), &expected[..len]);
    // What the radio receives is a ToRadio, not something we would take for a FromRadio result.
    assert_eq!(decode_from_radio(&message), Ok(FromRadio::Other));
}

#[test]
fn the_largest_packet_fits_the_message_buffer() {
    let payload = [9u8; 219];
    let link = Link::new();
    link.session_up(0);
    let mut radio = LinkRadio::new(&link);
    let packet = Outgoing {
        payload: &payload,
        ..outgoing(u32::MAX)
    };
    block_on(radio.send(&packet)).unwrap();
    assert!(block_on(link.next_outgoing()).len() <= TO_RADIO_MAX);
}

#[test]
fn telemetry_results_are_delivered_in_order_and_counted() {
    let link = Link::new();
    link.session_up(0);
    link.on_routing(5, result(1, 0));
    link.on_routing(5, result(2, 5));
    let mut radio = LinkRadio::new(&link);
    assert_eq!(block_on(radio.next_result()).unwrap().request_id, 1);
    assert_eq!(block_on(radio.next_result()).unwrap().request_id, 2);
    let snapshot = link.snapshot();
    assert_eq!(
        (snapshot.acked, snapshot.failed, snapshot.sessions),
        (1, 1, 1)
    );
    assert!(
        link.inbox().is_empty(),
        "telemetry results never reach the inbox"
    );
}

#[test]
fn ids_are_nonzero_consecutive_and_never_reused_after_the_range_wraps() {
    let link = Link::new();
    assert_eq!(link.reserve_ids(3), 1);
    assert_eq!(link.reserve_ids(1), 4);
    link.seed_ids(0);
    assert_eq!(link.reserve_ids(1), 1, "zero is never an id");
    link.seed_ids(u32::MAX - 2);
    assert_eq!(
        link.reserve_ids(16),
        1,
        "a range that would pass zero starts over at one"
    );
}

#[test]
fn a_diagnostic_ack_goes_to_the_inbox_and_never_to_the_transport() {
    let link = Link::new();
    link.session_up(0);
    link.note_diagnostic_send(10);
    link.on_routing(0xdead_beef, result(10, 0));
    link.on_routing(0xdead_beef, result(11, 0));
    let inbox = link.inbox();
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        (inbox[0].kind, inbox[0].from, inbox[0].packet_id),
        (InboxKind::Ack, Some(0xdead_beef), 10)
    );
    assert_eq!(inbox[0].text.as_str(), "ok");
    // The transport sees its own result and only that one, whether or not anybody read the inbox.
    let mut radio = LinkRadio::new(&link);
    assert_eq!(block_on(radio.next_result()).unwrap().request_id, 11);
    // A failure carries its code; a second ack of the same packet is not a diagnostic any more.
    link.note_diagnostic_send(12);
    link.on_routing(1, result(12, 5));
    assert_eq!(link.inbox()[0].text.as_str(), "error 5");
    link.on_routing(1, result(12, 0));
    assert_eq!(link.inbox().len(), 2);
    assert_eq!(block_on(radio.next_result()).unwrap().request_id, 12);
}

#[test]
fn the_inbox_is_a_newest_first_ring_of_six_that_reading_does_not_clear() {
    let link = Link::new();
    assert!(link.inbox().is_empty());
    for id in 1..=8 {
        link.on_text(0x10 + id, id, b"hi");
    }
    let inbox = link.inbox();
    let ids: std::vec::Vec<u32> = inbox.iter().map(|entry| entry.packet_id).collect();
    assert_eq!(ids, [8, 7, 6, 5, 4, 3], "the two oldest were overwritten");
    assert_eq!(link.inbox(), inbox, "reading takes nothing out");
}

#[test]
fn texts_that_cannot_be_kept_whole_are_skipped_and_nothing_else_changes() {
    let link = Link::new();
    link.on_text(1, 1, &[b'a'; INBOX_TEXT_MAX]);
    link.on_text(1, 2, &[b'a'; INBOX_TEXT_MAX + 1]);
    link.on_text(1, 3, &[0xff, 0xfe]);
    link.on_text(1, 4, "héllo".as_bytes());
    let ids: std::vec::Vec<u32> = link.inbox().iter().map(|entry| entry.packet_id).collect();
    assert_eq!(ids, [4, 1]);
}

#[test]
fn a_lost_session_ends_the_wait_and_drops_what_was_queued() {
    let link = Link::new();
    link.session_up(100);
    link.on_routing(1, result(1, 0));
    link.set_node(5);
    link.add_peer(6);
    link.add_peer(5); // the radio itself
    link.note_diagnostic_send(3);
    assert_eq!((link.snapshot().node, link.snapshot().peers), (Some(5), 1));
    link.session_down(Some(LastError::UartIo), None);
    let d = link.diagnostics();
    assert_eq!(
        (d.link.connected, d.link.node, d.link.peers, d.session_ms),
        (false, None, 0, None)
    );
    assert_eq!(d.last_error, LastError::UartIo);
    let mut radio = LinkRadio::new(&link);
    // The stale result is gone; waiting reports the loss instead of hanging.
    assert_eq!(block_on(radio.next_result()), Err(RadioError::Disconnected));
    // The acknowledgement of a send of the old session is not expected any more.
    link.on_routing(1, result(3, 0));
    assert!(link.inbox().is_empty());
    link.session_up(200);
    assert!(link.snapshot().connected);
}

#[test]
fn every_waiter_hears_that_the_session_ended() {
    use embassy_futures::join::join3;
    let link = Link::new();
    link.session_up(0);
    let (mut a, mut b) = (LinkRadio::new(&link), LinkRadio::new(&link));
    let ender = async {
        for _ in 0..5 {
            embassy_futures::yield_now().await;
        }
        link.session_down(Some(LastError::UartIo), None);
    };
    // Two waiters on the same link, as the transport and a diagnostic action are: both are woken.
    let (first, second, ()) = block_on(join3(a.next_result(), b.next_result(), ender));
    assert_eq!(first, Err(RadioError::Disconnected));
    assert_eq!(second, Err(RadioError::Disconnected));
}

#[test]
fn counters_saturate_and_a_reconnect_is_an_attempt_after_the_first() {
    let link = Link::new();
    link.session_begin();
    assert_eq!(
        link.diagnostics().reconnects,
        0,
        "the first attempt is not a reconnect"
    );
    link.session_up(0);
    link.note_tx();
    link.note_rx(10);
    link.note_framing_error();
    link.note_decode_error();
    link.session_down(Some(LastError::HandshakeTimeout), Some(false));
    link.session_begin();
    let d = link.diagnostics();
    assert_eq!(
        (
            d.tx_frames,
            d.rx_frames,
            d.framing_errors,
            d.decode_errors,
            d.reconnects
        ),
        (1, 1, 1, 1, 1)
    );
    assert_eq!(
        (d.handshake_failures, d.recovering, d.bad_streak),
        (1, true, 0)
    );
    assert_eq!(d.last_rx_ms, Some(10));
}

#[test]
fn a_metadata_text_is_kept_whole_and_bounded() {
    let link = Link::new();
    link.set_metadata("2.5.9.abcdef", 43);
    let d = link.diagnostics();
    assert_eq!(
        (d.firmware.as_str(), d.hardware),
        ("2.5.9.abcdef", Some(43))
    );
    link.set_metadata(&"é".repeat(40), 1);
    let d = link.diagnostics();
    assert!(d.firmware.len() <= FIRMWARE_MAX && d.firmware.chars().all(|c| c == 'é'));
}

#[test]
fn a_baud_change_asks_for_a_new_session_and_the_same_rate_does_not() {
    let link = Link::new();
    link.set_baud(115_200);
    assert!(!link.reconnect_pending());
    link.set_baud(9_600);
    assert_eq!(link.baud(), 9_600);
    assert!(link.reconnect_pending());
}
