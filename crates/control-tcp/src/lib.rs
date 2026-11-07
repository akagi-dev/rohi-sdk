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
#![no_std]

//! The ROHI control protocol over TCP: length-prefixed Protobuf records and the
//! serving loop of the device.
//!
//! # Overview
//!
//! `rohi-control-tcp` is the TCP transport of the Robonomics Open Hardware
//! Interface (ROHI) control protocol. The protocol itself is common: the same
//! `Request` and `Response` messages of [`rohi_control_proto`] are dispatched by
//! the same [`Controller`](rohi_control::Controller), with the same
//! authorization policy and the same registries as over BLE
//! (`rohi-control-ble`). Only the carrier differs, and TCP needs none of the BLE
//! machinery: a TCP stream is ordered and reliable, so there is no
//! fragmentation, no message id and no MTU. A message is a record with a 4-byte
//! length ([`frame`]).
//!
//! The crate is `no_std` and allocation free and contains no socket code. The
//! listening socket is an [`Acceptor`] supplied by a platform crate
//! (`rohi-net-std` on a host operating system). It is meant for development and
//! test setups (a `std` device on a host, a container, a bench), see
//! [Guarantees and Limitations](#guarantees-and-limitations).
//!
//! # Quick Example
//!
//! One `GetInfo` request travelling through [`ControlTcp`] over an in-memory
//! connection that is fed one byte at a time.
//!
//! ```
//! use core::convert::Infallible;
//! use std::cell::RefCell;
//! use std::rc::Rc;
//!
//! use embassy_futures::block_on;
//! use embedded_io_async::{ErrorType, Read, Write};
//! use rohi_control::{Controller, Device, DeviceInfo, OpenPolicy};
//! use rohi_control_proto::control::{self as pb, Request_::Request as Call, Response_::Response as Reply};
//! use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};
//! use rohi_control_tcp::frame::{HEADER_LEN, decode_header, encode_header};
//! use rohi_control_tcp::{Acceptor, ControlTcp};
//!
//! // A connection: the bytes the client sent and what the device wrote back.
//! #[derive(Default)]
//! struct Wire {
//!     incoming: Vec<u8>,
//!     outgoing: Vec<u8>,
//! }
//!
//! #[derive(Clone, Default)]
//! struct Connection(Rc<RefCell<Wire>>);
//!
//! impl ErrorType for Connection {
//!     type Error = Infallible;
//! }
//!
//! impl Read for Connection {
//!     async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Infallible> {
//!         // One byte per read: records may arrive in pieces.
//!         let mut wire = self.0.borrow_mut();
//!         if wire.incoming.is_empty() || buf.is_empty() {
//!             return Ok(0);
//!         }
//!         buf[0] = wire.incoming.remove(0);
//!         Ok(1)
//!     }
//! }
//!
//! impl Write for Connection {
//!     async fn write(&mut self, buf: &[u8]) -> Result<usize, Infallible> {
//!         self.0.borrow_mut().outgoing.extend_from_slice(buf);
//!         Ok(buf.len())
//!     }
//!
//!     async fn flush(&mut self) -> Result<(), Infallible> {
//!         Ok(())
//!     }
//! }
//!
//! // An acceptor with a single client.
//! struct OneClient(Option<Connection>);
//!
//! impl Acceptor for OneClient {
//!     type Stream = Connection;
//!     type Error = &'static str;
//!
//!     async fn accept(&mut self) -> Result<Connection, &'static str> {
//!         self.0.take().ok_or("no more clients")
//!     }
//! }
//!
//! // The client: a GetInfo request as one record.
//! let request = pb::Request {
//!     protocol_version: PROTOCOL_VERSION,
//!     request_id: 7,
//!     request: Some(Call::GetInfo(pb::GetInfoRequest {})),
//! };
//! let mut bytes = [0u8; 16];
//! let len = encode(&request, &mut bytes).unwrap();
//! let mut incoming = encode_header(len as u32).to_vec();
//! incoming.extend_from_slice(&bytes[..len]);
//! let connection = Connection::default();
//! connection.0.borrow_mut().incoming = incoming;
//!
//! // The device: a controller over a device, served to the client.
//! let info = DeviceInfo { firmware: "demo-0.1.0", board: "demo-board" };
//! let controller = Controller::new(Device::new(info, || 0, (), (), ()), OpenPolicy);
//! let mut control = ControlTcp::new(OneClient(Some(connection.clone())), &controller);
//! let error = block_on(control.run());
//! assert_eq!(error, "no more clients");
//!
//! // The client reads the response record.
//! let wire = connection.0.borrow();
//! let outgoing = &wire.outgoing;
//! let header: [u8; HEADER_LEN] = outgoing[..HEADER_LEN].try_into().unwrap();
//! let len = decode_header(header, 2048).unwrap();
//! let response = decode::<pb::Response>(&outgoing[HEADER_LEN..HEADER_LEN + len]).unwrap();
//! assert_eq!(response.request_id, 7);
//! let Some(Reply::GetInfo(info)) = response.response else { panic!() };
//! assert_eq!(info.board.as_str(), "demo-board");
//! ```
//!
//! # Core Concepts
//!
//! | Concept | Meaning |
//! |---------|---------|
//! | Record | A 4-byte little-endian length and one Protobuf message ([`frame`]) |
//! | [`Acceptor`] | Yields the next client connection as an `embedded-io-async` stream |
//! | [`ControlTcp`] | Accepts a client, then reads a request record, lets the [`Controller`](rohi_control::Controller) answer, writes the response record |
//! | Link security | Always the default (unencrypted, unauthenticated) one |
//!
//! **Connections.** One client is served at a time. While it is connected, the
//! next one waits in the backlog of the platform listener. A client that
//! disconnects and connects again starts from a clean state; nothing of the old
//! connection is kept.
//!
//! **Requests.** One request is in flight at a time and every request record
//! gets exactly one response record, in order. A request that is not valid
//! Protobuf, has the wrong protocol version or is not authorized is answered by
//! the [`Controller`](rohi_control::Controller) with a typed error response and
//! the connection stays open.
//!
//! # Architecture and Integration
//!
//! ```text
//!  rohi tool (client)                       device (std platform)
//!  ──────────────────                       ─────────────────────
//!  rohi_control_tcp::frame ──► TCP ──► Acceptor (rohi-net-std)
//!                                                │ embedded-io-async stream
//!                                                ▼
//!                                  ControlTcp ── frame
//!                                                │ request bytes + LinkSecurity
//!                                                ▼
//!                                  rohi_control::Controller ── registries of rohi-service
//!  response record ◄── TCP ◄── ControlTcp ◄──────┘
//! ```
//!
//! * The listener is not here: `rohi-net-std` implements [`Acceptor`] over host
//!   sockets. The wiring in the firmware is the `std` platform of
//!   `firmwares/altruist-urban`.
//! * BLE and TCP are independent transports of the same controller; a device
//!   may serve both with two tasks over one backend.
//! * The message schema is [`rohi_control_proto`].
//!
//! # API Navigation
//!
//! | Item | Use it to |
//! |------|-----------|
//! | [`ControlTcp`] | serve a controller to the clients of an acceptor |
//! | [`Acceptor`] | provide client connections from a platform |
//! | [`frame`] | encode and check record headers, on either side of the link |
//!
//! # Guarantees and Limitations
//!
//! * **Bounded memory.** All buffers are inline. The length of a record is
//!   checked against
//!   [`MAX_REQUEST_LEN`](rohi_control_proto::limits::MAX_REQUEST_LEN) before
//!   its payload is read, so an oversized request is never buffered.
//! * **Malformed records.** A header with length 0 or above the request limit
//!   cannot be skipped, because the stream could not be resynchronized. The
//!   device answers once with an `InvalidArgument` error (request id 0) and
//!   closes the connection. A connection that ends inside a header or a payload
//!   is dropped without a response.
//! * **Partial reads and writes.** Records may arrive in any number of pieces;
//!   the loop reads until a record is complete and writes until it is sent.
//! * **Cancellation.** Dropping [`ControlTcp::run`] or [`ControlTcp::serve_next`]
//!   closes the current connection and abandons the request being handled; what
//!   a service already committed stays committed.
//! * **No security.** The transport has no encryption, authentication or
//!   pairing, and [`OpenPolicy`](rohi_control::OpenPolicy) allows everything,
//!   including identity provisioning and erase. A platform must bind its
//!   listener to the loopback address unless the developer explicitly asks for
//!   more. It is a development transport, not a production-secure one.
//! * **Evidence.** The records and the serving loop are covered by host tests
//!   with scripted in-memory connections (`cargo test -p rohi-control-tcp`) and
//!   the example above. End-to-end use with host sockets and the `rohi` tool is
//!   covered by the tests of `firmwares/altruist-urban`.

#[cfg(test)]
extern crate std;

pub mod frame;
mod service;

#[cfg(test)]
mod tests;

pub use service::{Acceptor, ControlTcp};
