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

//! The ROHI control protocol over BLE: framing, GATT identifiers and the
//! serving loop of the peripheral.
//!
//! # Overview
//!
//! BLE moves small chunks (one ATT write or notification, at most
//! [`MAX_BLE_PAYLOAD`](rohi_net::MAX_BLE_PAYLOAD) = 244 bytes), while a control
//! message of the Robonomics Open Hardware Interface (ROHI) can be up to
//! [`MAX_REQUEST_LEN`](rohi_control_proto::limits::MAX_REQUEST_LEN) bytes for a
//! request and [`MAX_RESPONSE_LEN`](rohi_control_proto::limits::MAX_RESPONSE_LEN)
//! for a response. This crate bridges the two:
//!
//! * [`uuid`]: the GATT identifiers of the ROHI Control Service (one service,
//!   an RX characteristic the central writes and a TX characteristic the
//!   peripheral notifies);
//! * [`frame`]: a small framing with fragmentation and reassembly, independent
//!   of protobuf and bounded at compile time;
//! * [`ControlBle`]: the loop on the **peripheral** (firmware) side that
//!   receives chunks from a [`BleTransport`](rohi_net::BleTransport),
//!   reassembles a request, lets a [`Controller`](rohi_control::Controller)
//!   answer it and sends the response back in fragments.
//!
//! The crate is `no_std`, allocation free and contains no platform code. It is
//! not a BLE client: the host tool `rohi` (`tools/rohi`) is the central; it
//! reuses [`frame`] and [`uuid`] from this crate on its side of the link.
//!
//! # Quick Example
//!
//! One request travelling through [`ControlBle`] over a scripted in-memory
//! transport that carries 10 bytes per chunk, so both the request and the
//! response are fragmented. A real transport is `EspBle` of `rohi-net-esp`,
//! see [Architecture](#architecture-and-integration).
//!
//! ```
//! use std::cell::RefCell;
//! use std::collections::VecDeque;
//!
//! use embassy_futures::block_on;
//! use rohi_control::{Controller, Device, DeviceInfo, OpenPolicy};
//! use rohi_control_ble::ControlBle;
//! use rohi_control_ble::frame::{Fragmenter, Reassembler};
//! use rohi_control_proto::control::{self as pb, Request_::Request as Call, Response_::Response as Reply};
//! use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};
//! use rohi_net::{BleConnectionInfo, BleError, BleState, BleTransport};
//!
//! // The BLE side of the link: what the central wrote, what we notified.
//! struct Link {
//!     incoming: RefCell<VecDeque<Vec<u8>>>,
//!     outgoing: RefCell<Vec<Vec<u8>>>,
//! }
//!
//! impl BleTransport for &Link {
//!     async fn recv(&mut self, out: &mut [u8]) -> Result<usize, BleError> {
//!         let chunk = self.incoming.borrow_mut().pop_front().ok_or(BleError::Disconnected)?;
//!         out[..chunk.len()].copy_from_slice(&chunk);
//!         Ok(chunk.len())
//!     }
//!     async fn send(&mut self, data: &[u8]) -> Result<(), BleError> {
//!         self.outgoing.borrow_mut().push(data.to_vec());
//!         Ok(())
//!     }
//!     fn connection_info(&self) -> Option<BleConnectionInfo> {
//!         Some(BleConnectionInfo::default())
//!     }
//!     fn max_payload(&self) -> usize {
//!         10
//!     }
//!     fn state(&self) -> BleState {
//!         BleState::Advertising
//!     }
//!     fn connection_epoch(&self) -> u32 {
//!         1
//!     }
//! }
//!
//! // The central: a GetInfo request, framed with message id 5.
//! let request = pb::Request {
//!     protocol_version: PROTOCOL_VERSION,
//!     request_id: 1,
//!     request: Some(Call::GetInfo(pb::GetInfoRequest {})),
//! };
//! let mut bytes = [0u8; 16];
//! let len = encode(&request, &mut bytes).unwrap();
//! let mut chunk = [0u8; 10];
//! let mut frames = Fragmenter::new(5, &bytes[..len], 10).unwrap();
//! let link = Link { incoming: RefCell::default(), outgoing: RefCell::default() };
//! while let Some(n) = frames.next_frame(&mut chunk) {
//!     link.incoming.borrow_mut().push_back(chunk[..n].to_vec());
//! }
//! let chunks = link.incoming.borrow().len();
//! assert!(chunks > 1);
//!
//! // The firmware: a controller over a device, served over the link.
//! let info = DeviceInfo { firmware: "demo-0.1.0", board: "demo-board" };
//! let controller = Controller::new(Device::new(info, || 0, (), (), ()), OpenPolicy);
//! let mut ble = ControlBle::new(&link, &controller);
//! for _ in 0..chunks {
//!     block_on(ble.step());
//! }
//!
//! // The central reassembles the notified frames into the response.
//! let mut response = Reassembler::<256>::new();
//! let mut message = None;
//! for frame in link.outgoing.borrow().iter() {
//!     if let Some(done) = response.push(frame).unwrap() {
//!         assert_eq!(done.id, 5);
//!         message = Some(decode::<pb::Response>(done.bytes).unwrap());
//!     }
//! }
//! let Some(Reply::GetInfo(info)) = message.unwrap().response else { panic!() };
//! assert_eq!(info.board.as_str(), "demo-board");
//! ```
//!
//! # Core Concepts
//!
//! **GATT surface.** One primary service ([`uuid::SERVICE_UUID`]) with two
//! characteristics: the central writes request frames to
//! [`uuid::RX_CHARACTERISTIC_UUID`] and the peripheral sends response frames
//! as notifications of [`uuid::TX_CHARACTERISTIC_UUID`].
//!
//! **Frames.** A message (opaque bytes, protobuf in practice) is cut into
//! frames of at most the link payload, each with a 6-byte header (version,
//! message id, fragment index, fragment count, total length). The requester
//! picks the message id and the response frames echo it; frames of one message
//! arrive strictly in order and a new first fragment always starts a new
//! message. Details and errors are in [`frame`].
//!
//! **Serving.** [`ControlBle::step`] handles one transport event; the control
//! task calls it forever through [`ControlBle::run`]. One connection, one
//! request in flight, one response.
//!
//! # Architecture and Integration
//!
//! ```text
//!  central (rohi tool)                      peripheral (firmware)
//!  ───────────────────                      ─────────────────────
//!  rohi_control_ble::frame ──► RX write ──► BleTransport (EspBle, rohi-net-esp)
//!  rohi_control_ble::uuid                        │ recv / send / connection_info
//!                                                ▼
//!                                  ControlBle ── frame::Reassembler / Fragmenter
//!                                                │ request bytes + LinkSecurity
//!                                                ▼
//!                                  rohi_control::Controller ── registries of rohi-service
//!  response ◄── TX notifications ◄── frames ◄────┘
//! ```
//!
//! * The GATT server is not here: `rohi-net-esp` implements
//!   [`BleTransport`](rohi_net::BleTransport) for ESP32 and exposes the
//!   identifiers of [`uuid`]. The wiring in the firmware is
//!   `firmwares/altruist-urban/src/esp/control.rs`.
//! * The link security reported by the transport
//!   ([`BleConnectionInfo`](rohi_net::BleConnectionInfo)) becomes the
//!   [`LinkSecurity`](rohi_control::LinkSecurity) the authorization policy of
//!   [`rohi_control`] decides on. This crate itself implements neither
//!   pairing nor encryption.
//! * The message schema is [`rohi_control_proto`].
//!
//! # API Navigation
//!
//! * [`ControlBle`] - the serving loop ([`ControlBle::new`],
//!   [`ControlBle::step`], [`ControlBle::run`]).
//! * [`frame`] - [`Reassembler`](frame::Reassembler),
//!   [`Fragmenter`](frame::Fragmenter), [`Message`](frame::Message),
//!   [`FrameError`](frame::FrameError) and the constants of the format.
//! * [`uuid`] - the three GATT UUIDs.
//!
//! # Guarantees and Limitations
//!
//! * **Bounded memory.** All buffers are inline: a reassembly buffer of
//!   [`MAX_REQUEST_LEN`](rohi_control_proto::limits::MAX_REQUEST_LEN) bytes, a
//!   response buffer of [`MAX_RESPONSE_LEN`](rohi_control_proto::limits::MAX_RESPONSE_LEN)
//!   bytes and one chunk of [`MAX_BLE_PAYLOAD`](rohi_net::MAX_BLE_PAYLOAD) bytes.
//!   A message is at most 65535 bytes and 255 fragments on the wire format
//!   level; the reassembler of [`ControlBle`] accepts requests up to the
//!   request limit.
//! * **Malformed frames never reach the controller.** A rejected frame is
//!   answered once with an `InvalidArgument` error (request id 0, carried in
//!   a frame with the message id of the offending frame); the remaining
//!   fragments of that message are skipped silently.
//! * **Connection changes.** A disconnect or a failed receive or send drops
//!   the partial request. A response whose connection ended or was replaced
//!   while the request ran is dropped, not sent to another central.
//! * **Cancellation.** Dropping a [`ControlBle::step`] future abandons the
//!   request being handled and leaves no state that outlives it; what a service
//!   already committed stays committed, and a response that was already
//!   partly notified stays partial, which the central discards when it times
//!   out or starts a new request.
//! * **Evidence.** The framing and the serving loop are covered by host tests
//!   with a scripted mock transport (`cargo test -p rohi-control-ble`) and the
//!   examples on this page. The firmware integration builds in CI, but BLE on
//!   real hardware is *not verified*: the support matrix
//!   (`device-matrix.toml`, capability `ble-control`) records
//!   hardware verification as `not-verified`, and no pairing or encryption is
//!   implemented yet, so the default [`OpenPolicy`](rohi_control::OpenPolicy)
//!   offers no protection on the link. Advertising, coexistence with Wi-Fi,
//!   MTU behaviour and long-run stability are not measured.

#[cfg(test)]
extern crate std;

pub mod frame;
mod service;
pub mod uuid;

#[cfg(test)]
mod tests;

pub use service::ControlBle;
