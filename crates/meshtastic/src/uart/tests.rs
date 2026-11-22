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
use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_futures::select::{Either, select};
use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_time::MonotonicTimestamp;

use super::*;
use crate::link::{InboxKind, LinkRadio};
use crate::proto::PORT_TEXT_MESSAGE_APP;
use crate::radio::{MeshtasticRadio, Outgoing, RoutingResult};

// ---- protobuf builders for what a radio sends ----------------------------------------------

fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
    out
}

fn field(number: u32, wire: u8, body: &[u8]) -> Vec<u8> {
    let mut out = varint(u64::from(number) << 3 | u64::from(wire));
    if wire == 2 {
        out.extend(varint(body.len() as u64));
    }
    out.extend_from_slice(body);
    out
}

fn my_info(node: u32) -> Vec<u8> {
    field(3, 2, &field(1, 0, &varint(u64::from(node))))
}

fn node_info(node: u32) -> Vec<u8> {
    field(4, 2, &field(1, 0, &varint(u64::from(node))))
}

fn metadata(firmware: &str, model: u32) -> Vec<u8> {
    let mut body = field(1, 2, firmware.as_bytes());
    body.extend(field(9, 0, &varint(u64::from(model))));
    field(13, 2, &body)
}

fn config_complete(id: u32) -> Vec<u8> {
    field(7, 0, &varint(u64::from(id)))
}

fn packet(from: u32, id: u32, port: u64, payload: &[u8], request_id: u32) -> Vec<u8> {
    let mut data = field(1, 0, &varint(port));
    data.extend(field(2, 2, payload));
    if request_id != 0 {
        data.extend(field(6, 5, &request_id.to_le_bytes()));
    }
    let mut packet = field(1, 5, &from.to_le_bytes());
    packet.extend(field(4, 2, &data));
    packet.extend(field(6, 5, &id.to_le_bytes()));
    field(2, 2, &packet)
}

fn routing_ack(from: u32, request_id: u32, error: u64) -> Vec<u8> {
    packet(from, 99, 5, &field(3, 0, &varint(error)), request_id)
}

fn text(from: u32, id: u32, text: &str) -> Vec<u8> {
    packet(from, id, 1, text.as_bytes(), 0)
}

fn framed(message: &[u8]) -> Vec<u8> {
    let mut out = header(message.len()).to_vec();
    out.extend_from_slice(message);
    out
}

// ---- the frame parser -------------------------------------------------------------------------

fn parse_all(parser: &mut FrameParser, bytes: &[u8]) -> (Vec<Vec<u8>>, usize) {
    let (mut frames, mut bad) = (Vec::new(), 0);
    for byte in bytes {
        match parser.push(*byte) {
            Push::Pending => {}
            Push::BadHeader => bad += 1,
            Push::Frame(frame) => frames.push(frame.to_vec()),
        }
    }
    (frames, bad)
}

#[test]
fn frames_are_found_whole_one_byte_at_a_time_and_concatenated() {
    let mut parser = FrameParser::new();
    let stream = [
        framed(b"one"),
        framed(b"two two"),
        framed(&[0x94, 0xc3, 0x00, 0x01, 0x7f]),
    ]
    .concat();
    let (frames, bad) = parse_all(&mut parser, &stream);
    assert_eq!(bad, 0);
    assert_eq!(
        frames,
        [
            b"one".to_vec(),
            b"two two".to_vec(),
            [0x94, 0xc3, 0x00, 0x01, 0x7f].to_vec()
        ]
    );
}

#[test]
fn noise_between_frames_is_skipped_and_counted_but_not_an_error() {
    let mut parser = FrameParser::new();
    let stream = [
        b"boot log\r\n".to_vec(),
        framed(b"a"),
        b"more".to_vec(),
        framed(b"b"),
    ]
    .concat();
    let (frames, bad) = parse_all(&mut parser, &stream);
    assert_eq!((frames.len(), bad), (2, 0));
    assert_eq!(parser.skipped(), 14);
}

#[test]
fn an_impossible_length_is_a_framing_error_and_the_next_frame_is_still_found() {
    for length in [[0x00, 0x00], [0x02, 0x01], [0xff, 0xff]] {
        let mut parser = FrameParser::new();
        let mut stream = vec![0x94, 0xc3, length[0], length[1]];
        stream.extend(framed(b"good"));
        let (frames, bad) = parse_all(&mut parser, &stream);
        assert_eq!(bad, 1, "{length:?}");
        assert_eq!(frames, [b"good".to_vec()], "{length:?}");
    }
    // The largest legal frame.
    let mut parser = FrameParser::new();
    let (frames, bad) = parse_all(&mut parser, &framed(&[7u8; MAX_FRAME]));
    assert_eq!((frames.len(), bad), (1, 0));
}

#[test]
fn a_truncated_header_costs_only_itself() {
    let mut parser = FrameParser::new();
    // A header cut short by a new one: 0x94 0xc3 0x94 0xc3 len ...
    let mut stream = vec![0x94, 0xc3, 0x94];
    stream.extend(framed(b"next"));
    let (frames, bad) = parse_all(&mut parser, &stream);
    // The cut-short header is one framing error; the frame after it is found.
    assert_eq!(bad, 1);
    assert_eq!(frames, [b"next".to_vec()]);
}

#[test]
fn reset_forgets_a_partial_frame() {
    let mut parser = FrameParser::new();
    let partial = &framed(b"abcdef")[..7];
    assert!(parse_all(&mut parser, partial).0.is_empty());
    parser.reset();
    assert_eq!(parse_all(&mut parser, &framed(b"xy")).0, [b"xy".to_vec()]);
}

#[test]
fn the_header_is_big_endian() {
    assert_eq!(header(0x0102), [0x94, 0xc3, 0x01, 0x02]);
}

// ---- a fake UART and a fake radio --------------------------------------------------------------

#[derive(Default)]
struct Pipe {
    bytes: RefCell<VecDeque<u8>>,
    closed: Cell<bool>,
    failed: Cell<bool>,
}

impl Pipe {
    fn send(&self, bytes: &[u8]) {
        self.bytes.borrow_mut().extend(bytes.iter().copied());
    }

    fn take(&self) -> Vec<u8> {
        self.bytes.borrow_mut().drain(..).collect()
    }
}

#[derive(Debug)]
struct FakeError;

impl core::fmt::Display for FakeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("fake")
    }
}

impl core::error::Error for FakeError {}

impl embedded_io_async::Error for FakeError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::Other
    }
}

/// A UART: reads what the radio sent (`inbound`) and writes what the device sends (`outbound`).
struct Io<'a> {
    inbound: &'a Pipe,
    outbound: &'a Pipe,
    baud: &'a Cell<u32>,
    baud_fails: bool,
}

impl embedded_io_async::ErrorType for Io<'_> {
    type Error = FakeError;
}

impl embedded_io_async::Read for Io<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, FakeError> {
        loop {
            if self.inbound.failed.get() {
                return Err(FakeError);
            }
            {
                let mut bytes = self.inbound.bytes.borrow_mut();
                if !bytes.is_empty() {
                    let n = bytes.len().min(buf.len());
                    for slot in &mut buf[..n] {
                        *slot = bytes.pop_front().unwrap_or(0);
                    }
                    return Ok(n);
                }
            }
            if self.inbound.closed.get() {
                return Ok(0);
            }
            yield_now().await;
        }
    }
}

impl SetBaudRate for Io<'_> {
    fn set_baud_rate(&mut self, baud: u32) -> Result<(), BaudError> {
        if self.baud_fails {
            return Err(BaudError);
        }
        self.baud.set(baud);
        Ok(())
    }
}

impl embedded_io_async::Write for Io<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, FakeError> {
        self.outbound.send(buf);
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), FakeError> {
        Ok(())
    }
}

struct NeverDelay;

impl embedded_hal_async::delay::DelayNs for NeverDelay {
    async fn delay_ns(&mut self, _ns: u32) {
        core::future::pending::<()>().await
    }
}

/// Every wait ends at once and moves the clock by what it waited.
struct JumpDelay<'a>(&'a Cell<u64>);

impl embedded_hal_async::delay::DelayNs for JumpDelay<'_> {
    async fn delay_ns(&mut self, ns: u32) {
        // Let the other side of the test run before the time passes.
        yield_now().await;
        self.0.set(self.0.get() + u64::from(ns) / 1_000_000);
    }
}

struct Time<'a>(&'a Cell<u64>);

impl rohi_time::Clock for Time<'_> {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(self.0.get())
    }
}

type Link = MeshtasticLink<NoopRawMutex>;

struct World {
    link: Link,
    to_device: Pipe,
    from_device: Pipe,
    baud: Cell<u32>,
    now: Cell<u64>,
}

impl World {
    fn new() -> Self {
        Self {
            link: Link::new(),
            to_device: Pipe::default(),
            from_device: Pipe::default(),
            baud: Cell::new(0),
            now: Cell::new(1_000),
        }
    }

    fn io(&self) -> Io<'_> {
        Io {
            inbound: &self.to_device,
            outbound: &self.from_device,
            baud: &self.baud,
            baud_fails: false,
        }
    }

    fn session<'a>(
        &'a self,
        io: Io<'a>,
    ) -> UartSession<'a, NoopRawMutex, Io<'a>, NeverDelay, Time<'a>> {
        UartSession::new(&self.link, io, NeverDelay, Time(&self.now), 41)
    }

    /// What the device wrote so far, as the messages a radio would read.
    fn written(&self) -> Vec<Vec<u8>> {
        let mut parser = FrameParser::new();
        parse_all(&mut parser, &self.from_device.take()).0
    }

    /// The radio side of the handshake: dumps its configuration and completes it.
    async fn handshake(&self, node: u32, peers: &[u32], nonce: u32) {
        let mut stream = framed(&my_info(node));
        stream.extend(framed(&metadata("2.5.9.abcdef", 43)));
        for peer in peers {
            stream.extend(framed(&node_info(*peer)));
        }
        stream.extend(framed(&config_complete(nonce)));
        self.to_device.send(&stream);
        while !self.link.snapshot().connected {
            yield_now().await;
        }
    }
}

fn run<T>(
    session: impl core::future::Future<Output = (SessionError, bool)>,
    script: impl core::future::Future<Output = T>,
) -> T {
    match block_on(select(session, script)) {
        Either::Second(value) => value,
        Either::First((error, _)) => panic!("the session ended: {error:?}"),
    }
}

fn want_config_id(message: &[u8]) -> u32 {
    assert_eq!(message[0], 0x18, "ToRadio.want_config_id");
    let mut value = 0u32;
    for (i, byte) in message[1..].iter().enumerate() {
        value |= u32::from(byte & 0x7f) << (7 * i);
    }
    value
}

// ---- the session -------------------------------------------------------------------------------

#[test]
fn the_handshake_connects_and_learns_the_radio() {
    let world = World::new();
    let mut session = world.session(world.io());
    run(session.session(), async {
        // The device asks for the configuration first, at the configured rate.
        while world.from_device.bytes.borrow().is_empty() {
            yield_now().await;
        }
        let sent = world.written();
        assert_eq!(sent.len(), 1);
        let nonce = want_config_id(&sent[0]);
        assert_eq!(world.baud.get(), 115_200);
        assert!(
            !world.link.snapshot().connected,
            "not before the dump is complete"
        );

        world
            .handshake(0xdead_beef, &[0x11, 0xdead_beef, 0x22], nonce)
            .await;
        let d = world.link.diagnostics();
        assert_eq!(
            (d.link.node, d.link.peers, d.link.sessions),
            (Some(0xdead_beef), 2, 1)
        );
        assert_eq!(
            (d.firmware.as_str(), d.hardware),
            ("2.5.9.abcdef", Some(43))
        );
        assert_eq!((d.tx_frames, d.rx_frames), (1, 6));
        assert_eq!(d.session_ms, Some(1_000));
    });
}

#[test]
fn a_completion_with_another_number_does_not_connect() {
    let world = World::new();
    let mut session = world.session(world.io());
    run(session.session(), async {
        while world.from_device.bytes.borrow().is_empty() {
            yield_now().await;
        }
        let nonce = want_config_id(&world.written()[0]);
        world.to_device.send(&framed(&config_complete(nonce + 1)));
        for _ in 0..20 {
            yield_now().await;
        }
        assert!(!world.link.snapshot().connected);
        assert_eq!(world.link.diagnostics().rx_frames, 1);
    });
}

#[test]
fn text_and_acks_are_fanned_out_to_the_inbox_and_the_transport_independently() {
    let world = World::new();
    let mut session = world.session(world.io());
    run(session.session(), async {
        while world.from_device.bytes.borrow().is_empty() {
            yield_now().await;
        }
        let nonce = want_config_id(&world.written()[0]);
        world.handshake(1, &[], nonce).await;

        // A diagnostic send, a telemetry packet, then their acknowledgements and a text.
        world.link.note_diagnostic_send(500);
        let mut radio = LinkRadio::new(&world.link);
        let telemetry = radio.reserve_ids(1);
        let outgoing = Outgoing {
            destination: 7,
            id: telemetry,
            port: 256,
            payload: &[1, 2],
            want_ack: true,
            pki: true,
        };
        radio.send(&outgoing).await.unwrap();
        for _ in 0..10 {
            yield_now().await;
        }
        // The message reached the UART framed.
        assert_eq!(world.written().len(), 1);

        world.to_device.send(&framed(&routing_ack(0xaa, 500, 0)));
        world
            .to_device
            .send(&framed(&text(0xbb, 15230, "Hello ROHI")));
        world
            .to_device
            .send(&framed(&routing_ack(0xaa, telemetry, 0)));
        // Nobody read the inbox: the transport still gets its result.
        let result = radio.next_result().await.unwrap();
        assert_eq!(
            result,
            RoutingResult {
                request_id: telemetry,
                error: 0
            }
        );
        let inbox = world.link.inbox();
        let kinds: Vec<_> = inbox
            .iter()
            .map(|e| (e.kind, e.from, e.packet_id, e.text.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (InboxKind::Text, Some(0xbb), 15230, "Hello ROHI"),
                (InboxKind::Ack, Some(0xaa), 500, "ok"),
            ]
        );
        assert_eq!(PORT_TEXT_MESSAGE_APP, 1);
    });
}

#[test]
fn bad_frames_do_not_stop_the_session_and_are_counted() {
    let world = World::new();
    let mut session = world.session(world.io());
    run(session.session(), async {
        while world.from_device.bytes.borrow().is_empty() {
            yield_now().await;
        }
        let nonce = want_config_id(&world.written()[0]);
        world.handshake(1, &[], nonce).await;
        let rx_before = world.link.diagnostics().rx_frames;

        // An impossible length, a frame of garbage protobuf, then a valid message.
        world.to_device.send(&[0x94, 0xc3, 0x00, 0x00]);
        world.to_device.send(&framed(&[0x0b, 0x01]));
        world.to_device.send(&framed(&text(5, 1, "still works")));
        while world.link.inbox().is_empty() {
            yield_now().await;
        }
        let d = world.link.diagnostics();
        assert_eq!(
            (d.framing_errors, d.decode_errors, d.rx_frames),
            (1, 1, rx_before + 1)
        );
        assert_eq!(d.bad_streak, 0, "a valid message ends the streak");
        assert!(d.link.connected);
    });
}

#[test]
fn a_requested_reconnect_ends_the_session_and_fails_waiters_at_once() {
    let world = World::new();
    let mut session = world.session(world.io());
    let (error, _) = block_on(async {
        let script = async {
            while world.from_device.bytes.borrow().is_empty() {
                yield_now().await;
            }
            let nonce = want_config_id(&world.written()[0]);
            world.handshake(1, &[], nonce).await;
            let mut radio = LinkRadio::new(&world.link);
            world.link.request_reconnect();
            // The wait of the transport ends with a clear error instead of hanging.
            assert_eq!(
                radio.next_result().await,
                Err(crate::radio::RadioError::Disconnected)
            );
        };
        match select(session.session(), script).await {
            Either::First(ended) => ended,
            Either::Second(()) => panic!("the session should have ended first"),
        }
    });
    assert_eq!(error, SessionError::Requested);
}

#[test]
fn a_radio_restart_closed_line_and_io_error_end_the_session_with_their_own_reason() {
    for (kind, expected) in [
        (0, SessionError::Rebooted),
        (1, SessionError::Closed),
        (2, SessionError::Io),
    ] {
        let world = World::new();
        let mut session = world.session(world.io());
        let (error, _) = block_on(async {
            let script = async {
                while world.from_device.bytes.borrow().is_empty() {
                    yield_now().await;
                }
                match kind {
                    0 => world.to_device.send(&framed(&field(8, 0, &[1]))),
                    1 => world.to_device.closed.set(true),
                    _ => world.to_device.failed.set(true),
                }
                core::future::pending::<()>().await
            };
            match select(session.session(), script).await {
                Either::First(ended) => ended,
                Either::Second(()) => unreachable!(),
            }
        });
        assert_eq!(error, expected);
    }
}

#[test]
fn a_silent_radio_is_a_failed_handshake_and_a_chatty_one_is_not_configured() {
    // Silent: nothing came back.
    let world = World::new();
    let mut session = UartSession::new(
        &world.link,
        world.io(),
        JumpDelay(&world.now),
        Time(&world.now),
        1,
    );
    assert_eq!(
        block_on(session.session()),
        (SessionError::HandshakeTimeout, false)
    );
    assert!(world.now.get() >= 1_000 + HANDSHAKE_TIMEOUT_MS);

    // Text on the line, never a frame: the radio is not in PROTO mode.
    let world = World::new();
    world.to_device.send(b"INFO  | 12:00:00 booted\r\n");
    let mut session = UartSession::new(
        &world.link,
        world.io(),
        JumpDelay(&world.now),
        Time(&world.now),
        1,
    );
    assert_eq!(
        block_on(session.session()),
        (SessionError::HandshakeTimeout, true)
    );
}

#[test]
fn the_baud_rate_is_applied_when_a_session_starts_and_a_refusal_is_reported() {
    let world = World::new();
    world.link.set_baud(57_600);
    let mut session = world.session(world.io());
    // The pending request from `set_baud` belongs to the session that applies the new rate.
    run(session.session(), async {
        while world.from_device.bytes.borrow().is_empty() {
            yield_now().await;
        }
        assert_eq!(world.baud.get(), 57_600);
    });

    let world = World::new();
    let io = Io {
        baud_fails: true,
        ..world.io()
    };
    let mut session = world.session(io);
    assert_eq!(block_on(session.session()).0, SessionError::Baud);
    assert!(
        world.from_device.take().is_empty(),
        "nothing is sent at a rate that was not applied"
    );
}

#[test]
fn an_idle_session_sends_a_heartbeat_and_a_busy_one_is_not_kept_from_it() {
    let world = World::new();
    let mut session = UartSession::new(
        &world.link,
        world.io(),
        JumpDelay(&world.now),
        Time(&world.now),
        1,
    );
    let script = async {
        while world.from_device.bytes.borrow().is_empty() {
            yield_now().await;
        }
        let nonce = want_config_id(&world.written()[0]);
        world.to_device.send(&framed(&config_complete(nonce)));
        while !world.link.snapshot().connected {
            yield_now().await;
        }
        // With the jumping delay the heartbeat is due as soon as the session idles.
        while world.from_device.bytes.borrow().is_empty() {
            yield_now().await;
        }
        assert_eq!(world.written()[0], [0x3a, 0x00]);
        assert!(world.now.get() >= 1_000 + HEARTBEAT_MS);
    };
    run(session.session(), script);
}

#[test]
fn run_restarts_after_a_failure_and_counts_the_reconnect() {
    let world = World::new();
    let mut session = UartSession::new(
        &world.link,
        world.io(),
        JumpDelay(&world.now),
        Time(&world.now),
        1,
    );
    let script = async {
        // Nobody answers: handshakes time out and the session starts over, again and again.
        while world.link.diagnostics().handshake_failures < 3 {
            yield_now().await;
        }
        let d = world.link.diagnostics();
        assert!(d.reconnects >= 2 && d.last_error == LastError::HandshakeTimeout);
        assert!(!d.not_configured);
    };
    match block_on(select(session.run(), script)) {
        Either::Second(()) => {}
        Either::First(never) => match never {},
    }
}
