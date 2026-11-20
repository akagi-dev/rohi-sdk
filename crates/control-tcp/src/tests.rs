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

use core::convert::Infallible;
use core::time::Duration;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_futures::select::{Either, select};
use embedded_io_async::{ErrorType, Read, Write};
use rohi_control::{ControlAccess, ControlHub, Controller, Device, DeviceInfo, OpenPolicy};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::limits::{MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};

use crate::frame::{HEADER_LEN, decode_header, encode_header, plain_preamble};
use crate::{Acceptor, ControlTcp, Timed};

#[derive(Default)]
struct Wire {
    incoming: VecDeque<u8>,
    outgoing: Vec<u8>,
    /// The most a `read` returns.
    chunk: usize,
    /// A read that finds no data fails instead of ending the stream.
    reset_when_empty: bool,
    /// From this offset of `incoming` on (counted in bytes read), a byte takes `pace` to arrive.
    slow_from: usize,
    pace: Duration,
    read: usize,
    /// The virtual time, advanced by the slow bytes, and the absolute limit set by the server.
    now: Duration,
    deadline: Option<Duration>,
}

#[derive(Clone, Default)]
struct Connection(Rc<RefCell<Wire>>);

impl Connection {
    // A plaintext client: the preamble, then the records.
    fn plain(incoming: &[u8], chunk: usize) -> Self {
        Self::new(&[plain_preamble().as_slice(), incoming].concat(), chunk)
    }

    fn new(incoming: &[u8], chunk: usize) -> Self {
        let wire = Wire {
            incoming: incoming.iter().copied().collect(),
            chunk,
            slow_from: usize::MAX,
            ..Wire::default()
        };
        Self(Rc::new(RefCell::new(wire)))
    }

    // A client whose bytes from `slow_from` on come one at a time, each `pace` after the last.
    fn trickling(incoming: &[u8], slow_from: usize, pace: Duration) -> Self {
        let connection = Self::new(incoming, 4096);
        {
            let mut wire = connection.0.borrow_mut();
            wire.slow_from = slow_from;
            wire.pace = pace;
        }
        connection
    }

    fn outgoing(&self) -> Vec<u8> {
        self.0.borrow().outgoing.clone()
    }

    // How much of the script the server consumed.
    fn consumed(&self) -> usize {
        self.0.borrow().read
    }
}

impl Timed for Connection {
    fn set_deadline(&mut self, limit: Option<Duration>) {
        let mut wire = self.0.borrow_mut();
        wire.deadline = limit.map(|limit| wire.now + limit);
    }
}

#[derive(Debug)]
struct Reset(bool);

impl core::fmt::Display for Reset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("reset")
    }
}

impl core::error::Error for Reset {}

impl embedded_io_async::Error for Reset {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        if self.0 {
            embedded_io_async::ErrorKind::TimedOut
        } else {
            embedded_io_async::ErrorKind::ConnectionReset
        }
    }
}

impl ErrorType for Connection {
    type Error = Reset;
}

impl Read for Connection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Reset> {
        let mut wire = self.0.borrow_mut();
        if wire.incoming.is_empty() && wire.reset_when_empty {
            return Err(Reset(false));
        }
        let mut count = buf.len().min(wire.chunk.max(1)).min(wire.incoming.len());
        if wire.read < wire.slow_from {
            count = count.min(wire.slow_from - wire.read);
        } else if count > 0 {
            // A slow byte: the time passes, and a read after the limit is refused.
            count = 1;
            let pace = wire.pace;
            wire.now += pace;
            if wire.deadline.is_some_and(|deadline| wire.now > deadline) {
                return Err(Reset(true));
            }
        }
        for slot in &mut buf[..count] {
            *slot = wire.incoming.pop_front().unwrap();
        }
        wire.read += count;
        Ok(count)
    }
}

impl Write for Connection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Reset> {
        self.0.borrow_mut().outgoing.extend_from_slice(buf);
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Reset> {
        Ok(())
    }
}

struct Clients(VecDeque<Connection>);

impl Acceptor for Clients {
    type Stream = Connection;
    type Error = Infallible;

    async fn accept(&mut self) -> Result<Connection, Infallible> {
        match self.0.pop_front() {
            Some(connection) => Ok(connection),
            // The listener never fails; park the loop when the script is over.
            None => core::future::pending().await,
        }
    }
}

// The records of these tests are plaintext: the endpoint is open.
fn plain_access() -> ControlAccess {
    let access = ControlAccess::new(|_| false);
    access.set_mode(
        rohi_control::Transport::Tcp,
        rohi_control::TransportAccessMode::Open,
    );
    access
}

type Demo = Controller<Device<(), (), ()>, OpenPolicy>;

fn controller() -> Demo {
    let info = DeviceInfo {
        firmware: "demo-0.1.0",
        board: "demo-board",
    };
    Controller::new(Device::new(info, || 0, (), (), ()), OpenPolicy)
}

fn get_info(id: u32) -> Vec<u8> {
    let request = pb::Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: id,
        request: Some(Call::GetInfo(pb::GetInfoRequest {})),
    };
    let mut bytes = [0u8; 32];
    let len = encode(&request, &mut bytes).unwrap();
    record(&bytes[..len])
}

fn record(payload: &[u8]) -> Vec<u8> {
    let mut out = encode_header(payload.len() as u32).to_vec();
    out.extend_from_slice(payload);
    out
}

/// The response records of `bytes`.
fn responses(mut bytes: &[u8]) -> Vec<pb::Response> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let header: [u8; HEADER_LEN] = bytes[..HEADER_LEN].try_into().unwrap();
        let len = decode_header(header, MAX_RESPONSE_LEN).unwrap();
        out.push(decode(&bytes[HEADER_LEN..HEADER_LEN + len]).unwrap());
        bytes = &bytes[HEADER_LEN + len..];
    }
    out
}

fn is_error(response: &pb::Response, code: pb::ErrorCode) -> bool {
    matches!(&response.response, Some(Reply::Error(error)) if error.code == code)
}

/// Runs `future` next to the one control worker.
fn drive<T, B: rohi_control::ControlBackend>(
    hub: &ControlHub<'_, 2>,
    controller: &Controller<B, OpenPolicy>,
    future: impl core::future::Future<Output = T>,
) -> T {
    match block_on(select(hub.serve(controller), future)) {
        Either::First(never) => never,
        Either::Second(value) => value,
    }
}

/// Lets the worker finish what is queued, such as the end of a session.
fn settle<B: rohi_control::ControlBackend>(
    hub: &ControlHub<'_, 2>,
    controller: &Controller<B, OpenPolicy>,
) {
    drive(hub, controller, async {
        for _ in 0..4 {
            embassy_futures::yield_now().await;
        }
    });
}

/// Serves the connections in order, then stops.
fn serve(connections: &[&Connection]) {
    let controller = controller();
    let access = plain_access();
    let hub = ControlHub::<2>::with_access(&access);
    let clients = Clients(connections.iter().map(|c| (*c).clone()).collect());
    let mut control = ControlTcp::new(clients, &hub);
    for _ in connections {
        drive(&hub, &controller, control.serve_next()).unwrap();
    }
}

#[test]
fn a_request_is_answered_with_one_record() {
    let connection = Connection::plain(&get_info(5), 1024);
    serve(&[&connection]);
    let replies = responses(&connection.outgoing());
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].request_id, 5);
    assert!(matches!(replies[0].response, Some(Reply::GetInfo(_))));
}

#[test]
fn records_split_into_single_bytes_are_reassembled() {
    let connection = Connection::plain(&get_info(9), 1);
    serve(&[&connection]);
    let replies = responses(&connection.outgoing());
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].request_id, 9);
}

#[test]
fn requests_are_answered_in_order_on_one_connection() {
    let mut bytes = get_info(1);
    bytes.extend(get_info(2));
    bytes.extend(get_info(3));
    let connection = Connection::plain(&bytes, 7);
    serve(&[&connection]);
    let ids: Vec<u32> = responses(&connection.outgoing())
        .iter()
        .map(|r| r.request_id)
        .collect();
    assert_eq!(ids, [1, 2, 3]);
}

#[test]
fn malformed_protobuf_is_answered_and_the_connection_stays_open() {
    let mut bytes = record(&[0xff, 0xff, 0xff, 0xff, 0xff]);
    bytes.extend(get_info(4));
    let connection = Connection::plain(&bytes, 1024);
    serve(&[&connection]);
    let replies = responses(&connection.outgoing());
    assert_eq!(replies.len(), 2);
    assert!(is_error(&replies[0], pb::ErrorCode::InvalidArgument));
    assert_eq!(replies[0].request_id, 0);
    assert_eq!(replies[1].request_id, 4);
}

#[test]
fn an_oversized_record_is_rejected_before_its_payload_is_read() {
    // Only the header is on the wire: the payload is never read.
    let mut bytes = encode_header(MAX_REQUEST_LEN as u32 + 1).to_vec();
    bytes.extend(get_info(1));
    let connection = Connection::plain(&bytes, 1024);
    serve(&[&connection]);
    let replies = responses(&connection.outgoing());
    assert_eq!(replies.len(), 1);
    assert!(is_error(&replies[0], pb::ErrorCode::InvalidArgument));
    // The connection was closed: the bytes after the header were not consumed.
    assert_eq!(connection.0.borrow().incoming.len(), get_info(1).len());
}

#[test]
fn the_largest_request_is_accepted() {
    // Not valid Protobuf, but within the limit: it reaches the controller.
    let connection = Connection::plain(&record(&[0u8; MAX_REQUEST_LEN]), 1024);
    serve(&[&connection]);
    let replies = responses(&connection.outgoing());
    assert_eq!(replies.len(), 1);
    assert!(is_error(&replies[0], pb::ErrorCode::InvalidArgument));
}

#[test]
fn an_empty_record_is_rejected_and_closes_the_connection() {
    let mut bytes = encode_header(0).to_vec();
    bytes.extend(get_info(1));
    let connection = Connection::plain(&bytes, 1024);
    serve(&[&connection]);
    let replies = responses(&connection.outgoing());
    assert_eq!(replies.len(), 1);
    assert!(is_error(&replies[0], pb::ErrorCode::InvalidArgument));
}

#[test]
fn a_connection_that_ends_inside_a_payload_gets_no_response() {
    let mut bytes = get_info(1);
    bytes.truncate(bytes.len() - 1);
    let connection = Connection::plain(&bytes, 1024);
    serve(&[&connection]);
    assert!(connection.outgoing().is_empty());
}

#[test]
fn a_connection_that_ends_inside_a_header_gets_no_response() {
    let connection = Connection::plain(&get_info(1)[..2], 1024);
    serve(&[&connection]);
    assert!(connection.outgoing().is_empty());
}

#[test]
fn a_reset_connection_is_dropped_and_the_next_client_is_served() {
    let first = Connection::plain(&get_info(1)[..3], 1024);
    first.0.borrow_mut().reset_when_empty = true;
    let second = Connection::plain(&get_info(2), 1024);
    serve(&[&first, &second]);
    assert!(first.outgoing().is_empty());
    let replies = responses(&second.outgoing());
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].request_id, 2);
}

#[test]
fn each_client_starts_from_a_clean_state() {
    // The first client leaves half a record behind; it must not leak into the
    // second connection.
    let first = Connection::plain(&get_info(1)[..6], 1024);
    let second = Connection::plain(&get_info(2), 1024);
    serve(&[&first, &second]);
    let replies = responses(&second.outgoing());
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].request_id, 2);
}

#[test]
fn run_serves_until_the_acceptor_fails() {
    struct Failing(u8);

    impl Acceptor for Failing {
        type Stream = Connection;
        type Error = &'static str;

        async fn accept(&mut self) -> Result<Connection, &'static str> {
            match self.0 {
                0 => Err("listener closed"),
                _ => {
                    self.0 -= 1;
                    Ok(Connection::new(&[], 8))
                }
            }
        }
    }

    let controller = controller();
    let access = plain_access();
    let hub = ControlHub::<2>::with_access(&access);
    let mut control = ControlTcp::new(Failing(3), &hub);
    assert_eq!(drive(&hub, &controller, control.run()), "listener closed");
}

#[test]
fn a_request_with_no_free_slot_is_answered_busy_and_its_payload_is_skipped() {
    let controller = controller();
    let access = plain_access();
    let hub = ControlHub::<2>::with_access(&access);
    // Two BLE connections own both slots.
    let first = hub.connect(rohi_control::Transport::Ble).unwrap();
    let second = hub.connect(rohi_control::Transport::Ble).unwrap();
    let held = (first.request().unwrap(), second.request().unwrap());
    let wire = Connection::plain(&[get_info(1), get_info(2)].concat(), 4096);
    let mut control = ControlTcp::new(Clients([wire.clone()].into()), &hub);
    drive(&hub, &controller, control.serve_next()).unwrap();
    let answers = responses(&wire.outgoing());
    assert_eq!(answers.len(), 2);
    assert!(
        answers
            .iter()
            .all(|answer| is_error(answer, pb::ErrorCode::Busy))
    );
    assert_eq!(access.stats(rohi_control::Transport::Tcp).errors, 2);
    drop(held);
}

#[test]
fn a_client_beyond_the_session_limit_is_refused() {
    let controller = controller();
    let access = plain_access();
    let hub = ControlHub::<2>::with_access(&access);
    let live: Vec<_> = (0..rohi_control::MAX_SESSIONS)
        .map(|_| hub.connect(rohi_control::Transport::Ble).unwrap())
        .collect();
    let wire = Connection::plain(&get_info(1), 4096);
    let mut control = ControlTcp::new(Clients([wire.clone()].into()), &hub);
    drive(&hub, &controller, control.serve_next()).unwrap();
    assert!(wire.outgoing().is_empty());
    drop(live);
}

#[test]
fn every_tcp_connection_is_its_own_session_across_transports() {
    let controller = controller();
    let access = plain_access();
    let hub = ControlHub::<2>::with_access(&access);
    let ble = hub.connect(rohi_control::Transport::Ble).unwrap();
    let wire = Connection::plain(&get_info(1), 4096);
    let mut control = ControlTcp::new(Clients([wire.clone()].into()), &hub);
    drive(&hub, &controller, control.serve_next()).unwrap();
    assert_eq!(responses(&wire.outgoing()).len(), 1);
    // The BLE connection is untouched by the end of the TCP one.
    assert_eq!(access.stats(rohi_control::Transport::Ble).sessions, 1);
    assert_eq!(access.stats(rohi_control::Transport::Tcp).sessions, 0);
    drop(ble);
}

use core::cell::Cell;

use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, Service, StreamChunk,
    StreamFile, StreamInfo, Value,
};

#[derive(Service)]
struct Probe {
    closed: Cell<u32>,
    open: Cell<bool>,
}

impl Action for Probe {
    const ACTIONS: &'static [ActionDescriptor] =
        &[ActionDescriptor::new(1, "export", ActionAccess::Read).stream()];

    async fn invoke(
        &self,
        _: u32,
        _: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        self.open.set(true);
        let mut files = heapless::Vec::new();
        files
            .push(StreamFile {
                id: 1,
                name: heapless::String::try_from("a.log").unwrap(),
                size_bytes: 3,
            })
            .unwrap();
        out.stream(StreamInfo { id: 5, files })
            .map_err(|_| ActionError::Internal)
    }

    async fn read_stream(
        &self,
        stream: u32,
        _: u32,
        _: u64,
        buf: &mut [u8],
    ) -> Result<StreamChunk, ActionError> {
        if !self.open.get() || stream != 5 {
            return Err(ActionError::UnknownStream);
        }
        buf[..3].copy_from_slice(b"abc");
        Ok(StreamChunk { len: 3, eof: true })
    }

    async fn close_stream(&self, _: u32) -> Result<(), ActionError> {
        if self.open.replace(false) {
            self.closed.set(self.closed.get() + 1);
            Ok(())
        } else {
            Err(ActionError::UnknownStream)
        }
    }
}

fn call(id: u32, call: Call) -> Vec<u8> {
    let request = pb::Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: id,
        request: Some(call),
    };
    let mut bytes = [0u8; 64];
    let len = encode(&request, &mut bytes).unwrap();
    record(&bytes[..len])
}

fn export() -> Vec<u8> {
    call(
        1,
        Call::InvokeAction(pb::InvokeActionRequest {
            service_id: <Probe as Service>::ID,
            action_id: 1,
            args: heapless::Vec::new(),
        }),
    )
}

fn read_stream() -> Vec<u8> {
    call(
        2,
        Call::ReadStream(pb::ReadStreamRequest {
            service_id: <Probe as Service>::ID,
            stream_id: 5,
            file_id: 1,
            offset: 0,
        }),
    )
}

#[test]
fn a_stream_is_released_with_its_connection_and_never_reaches_the_next_client() {
    let probe = Probe {
        closed: Cell::new(0),
        open: Cell::new(false),
    };
    let info = DeviceInfo {
        firmware: "demo-0.1.0",
        board: "demo-board",
    };
    let controller = Controller::new(Device::new(info, || 0, (), (), (&probe,)), OpenPolicy);
    let first = Connection::plain(&[export(), read_stream()].concat(), 4096);
    let second = Connection::plain(&read_stream(), 4096);
    let clients = Clients([first.clone(), second.clone()].into());
    let access = plain_access();
    let hub = ControlHub::<2>::with_access(&access);
    let mut control = ControlTcp::new(clients, &hub);

    drive(&hub, &controller, control.serve_next()).unwrap();
    let answers = responses(&first.outgoing());
    assert!(matches!(answers[0].response, Some(Reply::ActionStream(_))));
    assert!(matches!(answers[1].response, Some(Reply::ReadStream(_))));
    settle(&hub, &controller);
    assert_eq!(
        probe.closed.get(),
        1,
        "the disconnect closes the streams of the session"
    );

    drive(&hub, &controller, control.serve_next()).unwrap();
    assert!(is_error(
        &responses(&second.outgoing())[0],
        pb::ErrorCode::NotFound
    ));
}

#[test]
fn a_stream_left_open_cannot_be_read_by_the_next_connection_even_if_it_lives() {
    // A transport that never reports the end of the first connection (the
    // future was dropped) still separates the sessions.
    let probe = Probe {
        closed: Cell::new(0),
        open: Cell::new(false),
    };
    let info = DeviceInfo {
        firmware: "demo-0.1.0",
        board: "demo-board",
    };
    let controller = Controller::new(Device::new(info, || 0, (), (), (&probe,)), OpenPolicy);
    let first = Connection::plain(&export(), 4096);
    let second = Connection::plain(&read_stream(), 4096);
    let clients = Clients([first.clone(), second.clone()].into());
    let access = plain_access();
    let hub = ControlHub::<2>::with_access(&access);
    let mut control = ControlTcp::new(clients, &hub);
    drive(&hub, &controller, control.serve_next()).unwrap();
    // The service still has the stream: pretend it was not told.
    probe.open.set(true);
    drive(&hub, &controller, control.serve_next()).unwrap();
    assert!(is_error(
        &responses(&second.outgoing())[0],
        pb::ErrorCode::NotFound
    ));
}

// ---------------------------------------------------------------------------
// The secure channel: protected and private endpoints
// ---------------------------------------------------------------------------

mod secure_endpoint {
    use core::cell::Cell;

    use rohi_control::{AccessPolicy, Transport, TransportAccessMode};

    use super::*;
    use crate::PskSource;
    use crate::error_kind;
    use crate::secure::{
        CLIENT_FINISH_LEN, CLIENT_HELLO_LEN, Channel, ClientHandshake, NONCE_LEN, PSK_LEN,
        RECORD_HEADER_LEN, SERVER_HELLO_LEN, TAG_LEN,
    };

    const KEY: [u8; PSK_LEN] = [0x5a; PSK_LEN];

    struct Key(Cell<Option<[u8; PSK_LEN]>>);

    impl PskSource for &Key {
        fn psk(&self) -> Option<[u8; PSK_LEN]> {
            self.0.get()
        }
    }

    fn entropy(out: &mut [u8]) -> bool {
        out.fill(0x22);
        true
    }

    fn protected_access() -> ControlAccess {
        let access = ControlAccess::new(entropy);
        access.set_tcp_psk(true);
        access
    }

    // A device whose access policy is the real one, to see what the link reaches.
    type Secured<'a> = Controller<Device<(), (), ()>, AccessPolicy<'a>>;

    fn secured(access: &ControlAccess) -> Secured<'_> {
        let info = DeviceInfo {
            firmware: "demo-0.1.0",
            board: "demo-board",
        };
        Controller::new(
            Device::new(info, || 0, (), (), ()),
            AccessPolicy::new(access),
        )
    }

    // What the client of a scripted connection sends, and the channel to read what comes back.
    struct Script {
        bytes: Vec<u8>,
        channel: Channel,
        server_hello: Option<[u8; SERVER_HELLO_LEN]>,
    }

    impl Script {
        // The device nonce of these tests is what `entropy` fills: 0x22.
        fn start(key: &[u8; PSK_LEN]) -> Self {
            let (client, hello) = ClientHandshake::start(key, &[0x11; NONCE_LEN]);
            // The answer of the device is computable here: it only depends on the key and the nonces.
            let (_, server_hello) =
                crate::secure::ServerHandshake::respond(key, &hello, &[0x22; NONCE_LEN]).unwrap();
            let (channel, finish) = match client.on_server_hello(&server_hello) {
                Ok(done) => done,
                Err(_) => panic!("the test client knows its own key"),
            };
            let mut bytes = hello.to_vec();
            bytes.extend_from_slice(&finish);
            Self {
                bytes,
                channel,
                server_hello: Some(server_hello),
            }
        }

        // The hello with the key of the client, and a proof that cannot verify.
        fn with_another_key() -> Self {
            let mut script = Self::start(&[0x77; PSK_LEN]);
            script.server_hello = None;
            script
        }

        fn seal(&mut self, payload: &[u8]) -> Vec<u8> {
            let mut data = payload.to_vec();
            let (header, tag) = self.channel.seal(&mut data).unwrap();
            [header.as_slice(), &data, &tag].concat()
        }

        fn send(&mut self, payload: &[u8]) {
            let record = self.seal(payload);
            self.bytes.extend_from_slice(&record);
        }

        fn request(&mut self, call: Call, id: u32) {
            let request = pb::Request {
                protocol_version: PROTOCOL_VERSION,
                request_id: id,
                request: Some(call),
            };
            let mut bytes = [0u8; 64];
            let len = encode(&request, &mut bytes).unwrap();
            self.send(&bytes[..len]);
        }

        // The responses the device wrote after its hello.
        fn responses(&mut self, written: &[u8]) -> Vec<pb::Response> {
            let mut rest = &written[SERVER_HELLO_LEN.min(written.len())..];
            let mut out = Vec::new();
            while !rest.is_empty() {
                let header: [u8; RECORD_HEADER_LEN] = rest[..RECORD_HEADER_LEN].try_into().unwrap();
                let len = self.channel.open_header(header, MAX_RESPONSE_LEN).unwrap();
                let mut data = rest[RECORD_HEADER_LEN..RECORD_HEADER_LEN + len].to_vec();
                let tag: [u8; TAG_LEN] = rest
                    [RECORD_HEADER_LEN + len..RECORD_HEADER_LEN + len + TAG_LEN]
                    .try_into()
                    .unwrap();
                self.channel.open(header, &mut data, &tag).unwrap();
                out.push(decode(&data).unwrap());
                rest = &rest[RECORD_HEADER_LEN + len + TAG_LEN..];
            }
            out
        }
    }

    // Serves one scripted connection to its end on a protected endpoint with `key`.
    fn serve_one(access: &ControlAccess, key: &Key, bytes: &[u8]) -> Connection {
        let controller = secured(access);
        let hub = ControlHub::<2>::with_access(access);
        let wire = Connection::new(bytes, 4096);
        let mut control = ControlTcp::with_psk(Clients([wire.clone()].into()), &hub, key);
        drive_secured(&hub, &controller, control.serve_next()).unwrap();
        wire
    }

    fn drive_secured<T>(
        hub: &ControlHub<'_, 2>,
        controller: &Secured<'_>,
        future: impl core::future::Future<Output = T>,
    ) -> T {
        match block_on(select(hub.serve(controller), future)) {
            Either::First(never) => never,
            Either::Second(value) => value,
        }
    }

    fn info(id: u32) -> Call {
        let _ = id;
        Call::GetInfo(pb::GetInfoRequest {})
    }

    fn reset() -> Call {
        // A mutation: it reaches a registry (and is answered NotFound there) only on a secured link.
        Call::ResetConfigSection(pb::ResetConfigSectionRequest::default())
    }

    #[test]
    fn a_request_with_no_free_slot_is_answered_busy_and_the_connection_ends() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let mut script = Script::start(&KEY);
        script.request(info(1), 1);
        script.request(info(2), 2);
        let controller = secured(&access);
        let hub = ControlHub::<2>::with_access(&access);
        let first = hub.connect(rohi_control::Transport::Ble).unwrap();
        let second = hub.connect(rohi_control::Transport::Ble).unwrap();
        let held = (first.request().unwrap(), second.request().unwrap());
        let wire = Connection::new(&script.bytes, 4096);
        let mut control = ControlTcp::with_psk(Clients([wire.clone()].into()), &hub, &key);
        drive_secured(&hub, &controller, control.serve_next()).unwrap();
        // The record counters of the two sides can no longer agree: one answer, then nothing.
        let answers = script.responses(&wire.outgoing());
        assert_eq!(answers.len(), 1);
        assert!(is_error(&answers[0], pb::ErrorCode::Busy));
        assert_eq!(access.stats(Transport::Tcp).last_error, error_kind::BUSY);
        drop(held);
    }

    #[test]
    fn an_authenticated_client_is_served_over_encrypted_records() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let mut script = Script::start(&KEY);
        script.request(info(1), 1);
        script.request(info(2), 2);
        let wire = serve_one(&access, &key, &script.bytes);
        let written = wire.outgoing();
        // The hello of the device, then the encrypted answers: no plaintext header anywhere.
        assert_eq!(&written[..5], b"ROHI\x01");
        assert_eq!(&written[..SERVER_HELLO_LEN], script.server_hello.unwrap());
        let answers = script.responses(&written);
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].request_id, 1);
        let Some(Reply::GetInfo(info)) = &answers[1].response else {
            panic!("{answers:?}")
        };
        assert_eq!(info.board.as_str(), "demo-board");
        assert!(
            !written.windows(10).any(|window| window == b"demo-board"),
            "the response is not in the clear"
        );
        assert_eq!(access.errors(), 0);
    }

    #[test]
    fn the_handshake_makes_the_link_encrypted_and_authenticated_for_the_policy() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let mut script = Script::start(&KEY);
        script.request(reset(), 7);
        let wire = serve_one(&access, &key, &script.bytes);
        let answers = script.responses(&wire.outgoing());
        // Allowed (and then NotFound: the demo device has no such section), not PermissionDenied.
        assert!(
            is_error(&answers[0], pb::ErrorCode::NotFound),
            "{answers:?}"
        );
        assert_eq!((access.requests(), access.errors()), (1, 0));
    }

    #[test]
    fn a_client_with_another_key_gets_nothing_and_nothing_is_processed() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let mut script = Script::with_another_key();
        script.request(reset(), 1);
        let wire = serve_one(&access, &key, &script.bytes);
        // The device answered the hello (it does not reveal what it knows about the key), then
        // read the client proof, found it wrong and ended the connection.
        let written = wire.outgoing();
        assert_eq!(written.len(), SERVER_HELLO_LEN);
        let stats = access.stats(Transport::Tcp);
        // One failed handshake is one error, not two.
        assert_eq!((access.requests(), access.errors()), (0, 1));
        assert_eq!(stats.last_error, error_kind::AUTH);
    }

    #[test]
    fn plaintext_records_without_a_preamble_are_never_served() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        // The framing of the open mode, and a handshake that is not one.
        for bytes in [
            get_info(1),
            [get_info(1), get_info(2)].concat(),
            std::vec![0u8; CLIENT_HELLO_LEN + CLIENT_FINISH_LEN],
        ] {
            let wire = serve_one(&access, &key, &bytes);
            assert!(
                wire.outgoing().is_empty(),
                "nothing is answered in the clear"
            );
        }
        assert_eq!(access.requests(), 0);
    }

    #[test]
    fn without_a_key_a_client_of_the_handshake_is_refused() {
        let access = ControlAccess::new(entropy);
        let key = Key(Cell::new(None));
        let mut script = Script::start(&KEY);
        script.request(info(1), 1);
        let wire = serve_one(&access, &key, &script.bytes);
        assert!(wire.outgoing().is_empty());
        assert_eq!(access.stats(Transport::Tcp).last_error, error_kind::NO_PSK);
    }

    #[test]
    fn without_a_random_source_the_client_is_refused() {
        let access = ControlAccess::new(|_| false);
        access.set_tcp_psk(true);
        let key = Key(Cell::new(Some(KEY)));
        let script = Script::start(&KEY);
        let wire = serve_one(&access, &key, &script.bytes);
        assert!(wire.outgoing().is_empty());
        assert_eq!(access.stats(Transport::Tcp).last_error, error_kind::NO_PSK);
    }

    #[test]
    fn a_tampered_record_ends_the_connection_without_a_response() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        for index in 0..(RECORD_HEADER_LEN + 5 + TAG_LEN) {
            let mut script = Script::start(&KEY);
            let good = script.bytes.len();
            script.request(info(1), 1);
            script.request(info(2), 2);
            let mut bytes = script.bytes.clone();
            bytes[good + index] ^= 0x01;
            let wire = serve_one(&access, &key, &bytes);
            assert_eq!(wire.outgoing().len(), SERVER_HELLO_LEN, "byte {index}");
        }
        assert_eq!(access.requests(), 0);
    }

    #[test]
    fn a_replayed_dropped_or_reordered_record_ends_the_connection() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let mut script = Script::start(&KEY);
        let handshake = script.bytes.clone();
        let first = script.seal(&[0x08, 0x01]);
        let second = script.seal(&[0x08, 0x02]);
        let third = script.seal(&[0x08, 0x03]);
        // Replay: the first record twice. The first is processed (a request that is not a Request
        // is answered with an error), the repetition ends the connection.
        for sequence in [
            [first.clone(), first.clone()].concat(),
            [second.clone()].concat(),
            [first.clone(), third.clone()].concat(),
            [first.clone(), third.clone(), second.clone()].concat(),
        ] {
            let bytes = [handshake.clone(), sequence].concat();
            let wire = serve_one(&access, &key, &bytes);
            let mut decoder = Script {
                bytes: Vec::new(),
                channel: Script::start(&KEY).channel,
                server_hello: None,
            };
            let answered = decoder.responses(&wire.outgoing());
            assert!(answered.len() <= 1, "{answered:?}");
        }
    }

    #[test]
    fn a_connection_that_ends_inside_a_record_gets_no_response() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let mut script = Script::start(&KEY);
        script.request(info(1), 1);
        for cut in [
            1,
            RECORD_HEADER_LEN,
            RECORD_HEADER_LEN + 3,
            script.bytes.len() - 1,
        ] {
            let wire = serve_one(
                &access,
                &key,
                &script.bytes[..script.bytes.len() - cut.min(script.bytes.len())],
            );
            let _ = wire;
        }
        let truncated = &script.bytes[..script.bytes.len() - 1];
        let wire = serve_one(&access, &key, truncated);
        assert_eq!(wire.outgoing().len(), SERVER_HELLO_LEN);
    }

    #[test]
    fn a_record_over_the_request_limit_ends_the_connection_in_silence() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let script = Script::start(&KEY);
        let mut bytes = script.bytes.clone();
        // A header that claims more than a request can be: the data is never read.
        let len = (MAX_REQUEST_LEN + 1) as u16;
        bytes.extend_from_slice(&[1, (len >> 8) as u8, len as u8]);
        bytes.extend_from_slice(&std::vec![0u8; MAX_REQUEST_LEN + 1 + TAG_LEN]);
        let wire = serve_one(&access, &key, &bytes);
        assert_eq!(wire.outgoing().len(), SERVER_HELLO_LEN);
        assert_eq!(access.stats(Transport::Tcp).last_error, error_kind::FRAME);
    }

    #[test]
    fn a_new_key_ends_the_connections_of_the_old_one() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let mut script = Script::start(&KEY);
        script.request(info(1), 1);
        script.request(info(2), 2);
        // A stream that provisions another key once the first answer was written.
        struct Rotating<'a> {
            inner: Connection,
            access: &'a ControlAccess,
        }
        impl ErrorType for Rotating<'_> {
            type Error = Reset;
        }
        impl Read for Rotating<'_> {
            async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Reset> {
                // The second record is requested after the first answer went out.
                if self.inner.outgoing().len() > SERVER_HELLO_LEN {
                    self.access.set_tcp_psk(true);
                }
                self.inner.read(buf).await
            }
        }
        impl Timed for Rotating<'_> {
            fn set_deadline(&mut self, limit: Option<Duration>) {
                self.inner.set_deadline(limit);
            }
        }
        impl Write for Rotating<'_> {
            async fn write(&mut self, buf: &[u8]) -> Result<usize, Reset> {
                self.inner.write(buf).await
            }
            async fn flush(&mut self) -> Result<(), Reset> {
                Ok(())
            }
        }
        struct One<'a>(Option<Rotating<'a>>);
        impl<'a> Acceptor for One<'a> {
            type Stream = Rotating<'a>;
            type Error = Infallible;
            async fn accept(&mut self) -> Result<Rotating<'a>, Infallible> {
                match self.0.take() {
                    Some(stream) => Ok(stream),
                    None => core::future::pending().await,
                }
            }
        }
        let wire = Connection::new(&script.bytes, 4096);
        let controller = secured(&access);
        let hub = ControlHub::<2>::with_access(&access);
        let mut control = ControlTcp::with_psk(
            One(Some(Rotating {
                inner: wire.clone(),
                access: &access,
            })),
            &hub,
            &key,
        );
        drive_secured(&hub, &controller, control.serve_next()).unwrap();
        // Only the first request was answered.
        assert_eq!(script.responses(&wire.outgoing()).len(), 1);
    }

    #[test]
    fn cancelling_the_serving_future_closes_the_session_and_leaves_nothing_behind() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let script = Script::start(&KEY);
        // A client that connects, authenticates and then says nothing.
        struct Silent(Connection);
        impl ErrorType for Silent {
            type Error = Reset;
        }
        impl Read for Silent {
            async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Reset> {
                if self.0.0.borrow().incoming.is_empty() {
                    core::future::pending::<()>().await;
                }
                self.0.read(buf).await
            }
        }
        impl Timed for Silent {
            fn set_deadline(&mut self, limit: Option<Duration>) {
                self.0.set_deadline(limit);
            }
        }
        impl Write for Silent {
            async fn write(&mut self, buf: &[u8]) -> Result<usize, Reset> {
                self.0.write(buf).await
            }
            async fn flush(&mut self) -> Result<(), Reset> {
                Ok(())
            }
        }
        struct One(Option<Silent>);
        impl Acceptor for One {
            type Stream = Silent;
            type Error = Infallible;
            async fn accept(&mut self) -> Result<Silent, Infallible> {
                match self.0.take() {
                    Some(stream) => Ok(stream),
                    None => core::future::pending().await,
                }
            }
        }
        let controller = secured(&access);
        let hub = ControlHub::<2>::with_access(&access);
        let mut control = ControlTcp::with_psk(
            One(Some(Silent(Connection::new(&script.bytes, 4096)))),
            &hub,
            &key,
        );
        // The future never completes; stop it after the worker had time to see the connection.
        let mut cancelled = false;
        drive_secured(&hub, &controller, async {
            let serving = control.serve_next();
            let waiting = async {
                for _ in 0..16 {
                    embassy_futures::yield_now().await;
                }
            };
            if let Either::Second(()) = select(serving, waiting).await {
                cancelled = true;
            }
            for _ in 0..8 {
                embassy_futures::yield_now().await;
            }
        });
        assert!(cancelled);
        assert_eq!(access.stats(Transport::Tcp).sessions, 0);
    }

    // A client of a protected endpoint that sends one byte every 400 ms from byte `from` on,
    // against limits of one second: no single read is slow, the whole is.
    fn trickle(bytes: &[u8], from: usize) -> (ControlAccess, Connection) {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let wire = Connection::trickling(bytes, from, Duration::from_millis(400));
        let controller = secured(&access);
        let hub = ControlHub::<2>::with_access(&access);
        let mut control = ControlTcp::with_psk(Clients([wire.clone()].into()), &hub, &key)
            .with_timeouts(Duration::from_secs(1), Duration::from_secs(1));
        drive_secured(&hub, &controller, control.serve_next()).unwrap();
        assert_eq!(access.stats(Transport::Tcp).sessions, 0);
        (access, wire)
    }

    #[test]
    fn a_client_that_trickles_the_handshake_is_cut_off_at_the_deadline() {
        let script = Script::start(&KEY);
        let (access, wire) = trickle(&script.bytes, 0);
        // The third byte after the first is the one that passes the second.
        assert!(wire.consumed() < CLIENT_HELLO_LEN);
        assert!(wire.outgoing().is_empty());
        assert_eq!(access.stats(Transport::Tcp).last_error, error_kind::IO);
        assert_eq!(access.errors(), 1);
    }

    #[test]
    fn a_client_that_trickles_a_request_is_cut_off_at_the_deadline() {
        let mut script = Script::start(&KEY);
        let handshake = script.bytes.len();
        script.request(info(1), 1);
        let (access, wire) = trickle(&script.bytes, handshake);
        // The handshake was served; the record never completed and got no answer.
        assert!(wire.consumed() < script.bytes.len());
        assert_eq!(wire.outgoing().len(), SERVER_HELLO_LEN);
        assert_eq!(access.stats(Transport::Tcp).last_error, error_kind::IO);
        assert_eq!(access.requests(), 0);
    }

    #[test]
    fn a_client_that_is_in_time_is_not_cut_off() {
        // The same pace, with limits that allow it.
        let mut script = Script::start(&KEY);
        let handshake = script.bytes.len();
        script.request(info(1), 1);
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let wire = Connection::trickling(&script.bytes, handshake, Duration::from_millis(2));
        let controller = secured(&access);
        let hub = ControlHub::<2>::with_access(&access);
        let mut control = ControlTcp::with_psk(Clients([wire.clone()].into()), &hub, &key)
            .with_timeouts(Duration::from_secs(1), Duration::from_secs(1));
        drive_secured(&hub, &controller, control.serve_next()).unwrap();
        assert_eq!(script.responses(&wire.outgoing()).len(), 1);
    }

    // The plaintext requests of a client: the preamble, then the records.
    fn plain_script(calls: &[Call]) -> Vec<u8> {
        let mut bytes = plain_preamble().to_vec();
        for (id, call) in calls.iter().enumerate() {
            let request = pb::Request {
                protocol_version: PROTOCOL_VERSION,
                request_id: id as u32 + 1,
                request: Some(call.clone()),
            };
            let mut payload = [0u8; 64];
            let len = encode(&request, &mut payload).unwrap();
            bytes.extend(record(&payload[..len]));
        }
        bytes
    }

    fn denied(response: &pb::Response) -> bool {
        is_error(response, pb::ErrorCode::PermissionDenied)
    }

    #[test]
    fn a_protected_endpoint_serves_plaintext_reads_and_refuses_plaintext_mutations() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let wire = serve_one(&access, &key, &plain_script(&[info(1), reset(), info(3)]));
        let answers = responses(&wire.outgoing());
        assert_eq!(answers.len(), 3);
        assert!(matches!(answers[0].response, Some(Reply::GetInfo(_))));
        assert!(denied(&answers[1]));
        // The refusal does not end the connection.
        assert!(matches!(answers[2].response, Some(Reply::GetInfo(_))));
        // Requests are counted, the refusal is not an error.
        assert_eq!((access.requests(), access.errors()), (3, 0));
    }

    #[test]
    fn a_protected_endpoint_serves_both_framings_and_only_proof_unlocks_a_mutation() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let plain = serve_one(&access, &key, &plain_script(&[reset()]));
        assert!(denied(&responses(&plain.outgoing())[0]));
        let mut script = Script::start(&KEY);
        script.request(reset(), 1);
        let secure = serve_one(&access, &key, &script.bytes);
        let answers = script.responses(&secure.outgoing());
        // Allowed, then NotFound: the demo device has no such section.
        assert!(
            is_error(&answers[0], pb::ErrorCode::NotFound),
            "{answers:?}"
        );
        assert_eq!((access.requests(), access.errors()), (2, 0));
    }

    #[test]
    fn an_open_endpoint_allows_plaintext_mutations_without_proof() {
        let access = protected_access();
        access.set_mode(Transport::Tcp, TransportAccessMode::Open);
        let key = Key(Cell::new(None));
        let wire = serve_one(&access, &key, &plain_script(&[reset()]));
        let answers = responses(&wire.outgoing());
        assert!(
            is_error(&answers[0], pb::ErrorCode::NotFound),
            "{answers:?}"
        );
        // A key is not needed to be served, and the authenticated way still works.
        let mut script = Script::start(&KEY);
        script.request(reset(), 1);
        let key = Key(Cell::new(Some(KEY)));
        let wire = serve_one(&access, &key, &script.bytes);
        assert_eq!(script.responses(&wire.outgoing()).len(), 1);
    }

    #[test]
    fn a_private_endpoint_never_serves_plaintext_and_does_not_count_it_as_an_error() {
        let access = protected_access();
        access.set_mode(Transport::Tcp, TransportAccessMode::Private);
        let key = Key(Cell::new(Some(KEY)));
        let wire = serve_one(&access, &key, &plain_script(&[info(1)]));
        assert!(wire.outgoing().is_empty());
        // Nothing after the first four bytes was read, let alone processed.
        assert_eq!(wire.consumed(), 4);
        assert_eq!((access.requests(), access.errors()), (0, 0));
        // The authenticated client is served, whatever it asks.
        let mut script = Script::start(&KEY);
        script.request(info(1), 1);
        script.request(reset(), 2);
        let wire = serve_one(&access, &key, &script.bytes);
        let answers = script.responses(&wire.outgoing());
        assert!(matches!(answers[0].response, Some(Reply::GetInfo(_))));
        assert!(
            is_error(&answers[1], pb::ErrorCode::NotFound),
            "{answers:?}"
        );
    }

    #[test]
    fn an_unknown_preamble_or_version_ends_the_connection_and_counts_once() {
        let key = Key(Cell::new(Some(KEY)));
        let mut wrong_version = plain_preamble().to_vec();
        wrong_version[4] ^= 0xff;
        wrong_version.extend(get_info(1));
        // Not a preamble: a request in the old plaintext framing, text, and a hello of another
        // protocol.
        for bytes in [
            get_info(1),
            b"GET / HTTP/1.1\r\n\r\n".to_vec(),
            b"ROHX\x01".to_vec(),
            wrong_version,
        ] {
            let access = protected_access();
            let wire = serve_one(&access, &key, &bytes);
            assert!(wire.outgoing().is_empty(), "{bytes:?}");
            assert_eq!((access.requests(), access.errors()), (0, 1), "{bytes:?}");
            assert_eq!(access.stats(Transport::Tcp).last_error, error_kind::FRAME);
        }
    }

    #[test]
    fn a_failed_handshake_never_falls_back_to_plaintext() {
        let access = protected_access();
        let key = Key(Cell::new(Some(KEY)));
        let mut script = Script::with_another_key();
        // After the handshake that cannot verify, a client tries plain records anyway.
        script.bytes.extend(plain_script(&[info(1)]));
        let wire = serve_one(&access, &key, &script.bytes);
        assert_eq!(wire.outgoing().len(), SERVER_HELLO_LEN);
        assert_eq!((access.requests(), access.errors()), (0, 1));
    }

    #[test]
    fn a_missing_key_leaves_plaintext_reads_and_counts_a_handshake_as_one_error() {
        let access = ControlAccess::new(entropy);
        let key = Key(Cell::new(None));
        let wire = serve_one(&access, &key, &plain_script(&[info(1), reset()]));
        let answers = responses(&wire.outgoing());
        assert!(matches!(answers[0].response, Some(Reply::GetInfo(_))));
        assert!(denied(&answers[1]));
        let mut script = Script::start(&KEY);
        script.request(info(1), 1);
        let wire = serve_one(&access, &key, &script.bytes);
        assert!(wire.outgoing().is_empty());
        assert_eq!(access.errors(), 1);
    }
}
