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
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::vec::Vec;

use embassy_futures::block_on;
use embedded_io_async::{ErrorType, Read, Write};
use rohi_control::{Controller, Device, DeviceInfo, OpenPolicy};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::limits::{MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};

use crate::frame::{HEADER_LEN, decode_header, encode_header};
use crate::{Acceptor, ControlTcp};

#[derive(Default)]
struct Wire {
    incoming: VecDeque<u8>,
    outgoing: Vec<u8>,
    /// The most a `read` returns.
    chunk: usize,
    /// A read that finds no data fails instead of ending the stream.
    reset_when_empty: bool,
}

#[derive(Clone, Default)]
struct Connection(Rc<RefCell<Wire>>);

impl Connection {
    fn new(incoming: &[u8], chunk: usize) -> Self {
        let wire = Wire {
            incoming: incoming.iter().copied().collect(),
            chunk,
            ..Wire::default()
        };
        Self(Rc::new(RefCell::new(wire)))
    }

    fn outgoing(&self) -> Vec<u8> {
        self.0.borrow().outgoing.clone()
    }
}

#[derive(Debug)]
struct Reset;

impl core::fmt::Display for Reset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("reset")
    }
}

impl core::error::Error for Reset {}

impl embedded_io_async::Error for Reset {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::ConnectionReset
    }
}

impl ErrorType for Connection {
    type Error = Reset;
}

impl Read for Connection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Reset> {
        let mut wire = self.0.borrow_mut();
        if wire.incoming.is_empty() && wire.reset_when_empty {
            return Err(Reset);
        }
        let count = buf.len().min(wire.chunk.max(1)).min(wire.incoming.len());
        for slot in &mut buf[..count] {
            *slot = wire.incoming.pop_front().unwrap();
        }
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

/// Serves the connections in order, then stops.
fn serve(connections: &[&Connection]) {
    let controller = controller();
    let clients = Clients(connections.iter().map(|c| (*c).clone()).collect());
    let mut control = ControlTcp::new(clients, &controller);
    for _ in connections {
        block_on(control.serve_next()).unwrap();
    }
}

#[test]
fn a_request_is_answered_with_one_record() {
    let connection = Connection::new(&get_info(5), 1024);
    serve(&[&connection]);
    let replies = responses(&connection.outgoing());
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].request_id, 5);
    assert!(matches!(replies[0].response, Some(Reply::GetInfo(_))));
}

#[test]
fn records_split_into_single_bytes_are_reassembled() {
    let connection = Connection::new(&get_info(9), 1);
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
    let connection = Connection::new(&bytes, 7);
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
    let connection = Connection::new(&bytes, 1024);
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
    let connection = Connection::new(&bytes, 1024);
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
    let connection = Connection::new(&record(&[0u8; MAX_REQUEST_LEN]), 1024);
    serve(&[&connection]);
    let replies = responses(&connection.outgoing());
    assert_eq!(replies.len(), 1);
    assert!(is_error(&replies[0], pb::ErrorCode::InvalidArgument));
}

#[test]
fn an_empty_record_is_rejected_and_closes_the_connection() {
    let mut bytes = encode_header(0).to_vec();
    bytes.extend(get_info(1));
    let connection = Connection::new(&bytes, 1024);
    serve(&[&connection]);
    let replies = responses(&connection.outgoing());
    assert_eq!(replies.len(), 1);
    assert!(is_error(&replies[0], pb::ErrorCode::InvalidArgument));
}

#[test]
fn a_connection_that_ends_inside_a_payload_gets_no_response() {
    let mut bytes = get_info(1);
    bytes.truncate(bytes.len() - 1);
    let connection = Connection::new(&bytes, 1024);
    serve(&[&connection]);
    assert!(connection.outgoing().is_empty());
}

#[test]
fn a_connection_that_ends_inside_a_header_gets_no_response() {
    let connection = Connection::new(&get_info(1)[..2], 1024);
    serve(&[&connection]);
    assert!(connection.outgoing().is_empty());
}

#[test]
fn a_reset_connection_is_dropped_and_the_next_client_is_served() {
    let first = Connection::new(&get_info(1)[..3], 1024);
    first.0.borrow_mut().reset_when_empty = true;
    let second = Connection::new(&get_info(2), 1024);
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
    let first = Connection::new(&get_info(1)[..6], 1024);
    let second = Connection::new(&get_info(2), 1024);
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
    let mut control = ControlTcp::new(Failing(3), &controller);
    assert_eq!(block_on(control.run()), "listener closed");
}

#[test]
fn the_response_buffer_covers_the_largest_response() {
    // The header and the payload are written with one `write_all`.
    assert_eq!(
        core::mem::size_of::<[u8; HEADER_LEN + MAX_RESPONSE_LEN]>(),
        HEADER_LEN + MAX_RESPONSE_LEN
    );
}

use core::cell::Cell;

use rohi_service::{
    Action, ActionDescriptor, ActionError, ActionOutput, Service, StreamChunk, StreamFile,
    StreamInfo, Value,
};

#[derive(Service)]
struct Probe {
    lost: Cell<u32>,
    open: Cell<bool>,
}

impl Action for Probe {
    const ACTIONS: &'static [ActionDescriptor] = &[ActionDescriptor::new(1, "export").stream()];

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
        Err(ActionError::UnknownStream)
    }

    fn link_lost(&self) {
        self.lost.set(self.lost.get() + 1);
        self.open.set(false);
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
        lost: Cell::new(0),
        open: Cell::new(false),
    };
    let info = DeviceInfo {
        firmware: "demo-0.1.0",
        board: "demo-board",
    };
    let controller = Controller::new(Device::new(info, || 0, (), (), (&probe,)), OpenPolicy);
    let first = Connection::new(&[export(), read_stream()].concat(), 4096);
    let second = Connection::new(&read_stream(), 4096);
    let clients = Clients([first.clone(), second.clone()].into());
    let mut control = ControlTcp::new(clients, &controller);

    block_on(control.serve_next()).unwrap();
    let answers = responses(&first.outgoing());
    assert!(matches!(answers[0].response, Some(Reply::ActionStream(_))));
    assert!(matches!(answers[1].response, Some(Reply::ReadStream(_))));
    assert_eq!(probe.lost.get(), 1, "the disconnect releases the streams");

    block_on(control.serve_next()).unwrap();
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
        lost: Cell::new(0),
        open: Cell::new(false),
    };
    let info = DeviceInfo {
        firmware: "demo-0.1.0",
        board: "demo-board",
    };
    let controller = Controller::new(Device::new(info, || 0, (), (), (&probe,)), OpenPolicy);
    let first = Connection::new(&export(), 4096);
    let second = Connection::new(&read_stream(), 4096);
    let clients = Clients([first.clone(), second.clone()].into());
    let mut control = ControlTcp::new(clients, &controller);
    block_on(control.serve_next()).unwrap();
    // The service still has the stream: pretend it was not told.
    probe.open.set(true);
    block_on(control.serve_next()).unwrap();
    assert!(is_error(
        &responses(&second.outgoing())[0],
        pb::ErrorCode::NotFound
    ));
}
