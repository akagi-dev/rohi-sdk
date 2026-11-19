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
use log::{debug, info, trace, warn};
use rohi_control::{Busy, Connection, ControlHub, Reservation, Transport};
use rohi_control_proto::control::{self as pb, Response_::Response as Reply};
use rohi_control_proto::limits::MAX_REQUEST_LEN;
use rohi_control_proto::{PROTOCOL_VERSION, encode};
use rohi_net::{BleError, BleTransport, MAX_BLE_PAYLOAD};

use crate::frame::{Assembler, Fragmenter};

/// The BLE frontend of the control worker.
///
/// ```text
/// transport.recv -> Assembler -> slot of the ControlHub ==> control_task (Controller)
///                                                                |
/// transport.send <- Fragmenter <- slot of the ControlHub <=======+
/// ```
///
/// It owns the connection, the fragmentation and the flow control and nothing
/// else: it never calls the [`Controller`](rohi_control::Controller). A request
/// is reassembled straight into a request slot of the [`ControlHub`], handed
/// to the one worker ([`ControlHub::serve`]) and the response is fragmented
/// from the same slot, so the frontend holds only the reassembly state and
/// one chunk of [`MAX_BLE_PAYLOAD`] bytes. Nothing here runs from a radio
/// callback: the transport queues chunks and this loop does the work.
///
/// One connection, one request in flight, one response. Every connection is
/// its own [`session`](rohi_control::LinkSecurity::session) of the hub, so it
/// cannot use the streams of another transport or central.
/// The security of the link reported by [`BleTransport::connection_info`]
/// reaches the policy of the worker; a link without a connection is treated as
/// unencrypted and unauthenticated. BLE keeps its own link security (pairing and
/// encryption of the host stack) and adds no framing encryption of its own.
///
/// The [crate documentation](crate) has a complete example over an in-memory
/// transport.
pub struct ControlBle<'h, 'a, T, const SLOTS: usize> {
    transport: T,
    hub: &'h ControlHub<'a, SLOTS>,
    connection: Option<Connection<'h, 'a, SLOTS>>,
    reservation: Option<Reservation<'h, 'a, SLOTS>>,
    assembler: Assembler,
    chunk: [u8; MAX_BLE_PAYLOAD],
    announced_epoch: Option<u32>,
}

/// The kinds of error the frontend reports to the endpoint status.
pub mod error_kind {
    /// A frame was rejected by the framing.
    pub const FRAME: u8 = 1;
    /// The transport failed to receive or send.
    pub const TRANSPORT: u8 = 2;
    /// The hub had no slot or no session for the request.
    pub const BUSY: u8 = 3;
}

impl<'h, 'a, T, const SLOTS: usize> ControlBle<'h, 'a, T, SLOTS>
where
    T: BleTransport,
{
    /// Serves the clients of `transport` through `hub`.
    pub const fn new(transport: T, hub: &'h ControlHub<'a, SLOTS>) -> Self {
        Self {
            transport,
            hub,
            connection: None,
            reservation: None,
            assembler: Assembler::new(),
            chunk: [0; MAX_BLE_PAYLOAD],
            announced_epoch: None,
        }
    }

    /// The wrapped transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Serves requests forever. Failures end the current exchange only;
    /// the next connection starts from a clean state.
    ///
    /// Never returns; cancel it by dropping the future (see
    /// [`step`](Self::step) for what that means).
    pub async fn run(&mut self) -> ! {
        loop {
            self.step().await;
        }
    }

    /// Waits for one transport event and handles it: a frame, which may
    /// complete a request and send its response, or a disconnect.
    ///
    /// Cancellation: dropping the future abandons the request being handled
    /// (no response is sent for it) and leaves no partial state: a new first
    /// fragment always starts a new request. A request that already runs in
    /// the worker finishes, so operations the services committed stay
    /// committed; it is never repeated.
    pub async fn step(&mut self) {
        let received = self.transport.recv(&mut self.chunk).await;
        let len = match received {
            Ok(len) => len,
            Err(BleError::Disconnected) => {
                info!(target: "control.ble", "Central disconnected");
                self.announced_epoch = None;
                self.drop_request();
                if let Some(connection) = self.connection.take() {
                    connection.close().await;
                }
                return;
            }
            Err(error) => {
                warn!(target: "control.ble", "Receive failed (err={})", error);
                self.hub
                    .transport_error(Transport::Ble, error_kind::TRANSPORT);
                self.drop_request();
                return;
            }
        };

        let epoch = self.transport.connection_epoch();
        if self.announced_epoch != Some(epoch) {
            self.announced_epoch = Some(epoch);
            self.drop_request();
            if let Some(old) = self.connection.take() {
                old.close().await;
            }
            match self.hub.connect(Transport::Ble) {
                Ok(connection) => self.connection = Some(connection),
                Err(_) => warn!(target: "control.ble", "Too many connections, central ignored"),
            }
            info!(target: "control.ble", "Central connected (epoch={})", epoch);
        }
        let Some(connection) = self.connection.as_ref() else {
            return;
        };
        let (encrypted, authenticated) = self
            .transport
            .connection_info()
            .map_or((false, false), |info| (info.encrypted, info.authenticated));
        connection.set_link(encrypted, authenticated);
        trace!(target: "control.ble", "Frame received (size_b={})", len);

        let is_first = len > 2 && self.chunk[2] == 0;
        if self.reservation.is_none() && is_first {
            match connection.request() {
                Ok(reservation) => self.reservation = Some(reservation),
                Err(Busy::Busy | Busy::Closed) => {
                    let id = self.chunk[1];
                    warn!(target: "control.ble", "No request slot, request refused (id={})", id);
                    self.hub.transport_error(Transport::Ble, error_kind::BUSY);
                    self.send_error(id, pb::ErrorCode::Busy, epoch).await;
                    return;
                }
            }
        }
        // Without a request slot only the framing checks run: the rest of a
        // message that was refused is skipped and a stray frame is answered.
        let outcome = match self.reservation.as_mut() {
            Some(reservation) => self
                .assembler
                .push(reservation.buffer(), &self.chunk[..len]),
            None => self.assembler.push(&mut [], &self.chunk[..len]),
        };
        match outcome {
            Ok(None) => {}
            Ok(Some((id, size))) => {
                let Some(reservation) = self.reservation.take() else {
                    return;
                };
                // Trace, not debug: a client that follows the log tail polls every
                // second, and a record per request would feed the tail it reads.
                trace!(
                    target: "control.ble",
                    "Request reassembled (id={} size_b={})",
                    id,
                    size
                );
                let response = reservation.submit(size).await;
                if !response.bytes().is_empty() {
                    self.send_message(id, response.bytes(), epoch).await;
                }
            }
            Err(error) => {
                warn!(target: "control.ble", "Frame rejected (err={})", error);
                self.hub.transport_error(Transport::Ble, error_kind::FRAME);
                self.reservation = None;
                let id = self.assembler.last_id();
                self.send_error(id, pb::ErrorCode::InvalidArgument, epoch)
                    .await;
            }
        }
    }

    fn drop_request(&mut self) {
        self.assembler.reset();
        self.reservation = None;
    }

    async fn send_error(&mut self, id: u8, code: pb::ErrorCode, epoch: u32) {
        let response = pb::Response {
            protocol_version: PROTOCOL_VERSION,
            request_id: 0,
            response: Some(Reply::Error(pb::Error {
                code,
                ..Default::default()
            })),
        };
        let mut bytes = [0u8; 16];
        match encode(&response, &mut bytes) {
            Ok(written) => self.send_message(id, &bytes[..written], epoch).await,
            Err(error) => {
                warn!(target: "control.ble", "Cannot encode the error response (err={:?})", error)
            }
        }
    }

    /// Sends `message` unless the connection changed meanwhile.
    async fn send_message(&mut self, id: u8, message: &[u8], epoch: u32) {
        if self.transport.connection_info().is_none() || self.transport.connection_epoch() != epoch
        {
            debug!(
                target: "control.ble",
                "Link changed, response dropped (id={} size_b={})",
                id, message.len()
            );
            self.drop_request();
            return;
        }
        let max = self.transport.max_payload().min(MAX_BLE_PAYLOAD);
        let mut fragments = match Fragmenter::new(id, message, max) {
            Ok(fragments) => fragments,
            Err(error) => {
                warn!(
                    target: "control.ble",
                    "Cannot frame the response (id={} size_b={} err={})",
                    id, message.len(), error
                );
                return;
            }
        };
        trace!(
            target: "control.ble",
            "Sending response (id={} size_b={} frames={})",
            id,
            message.len(),
            fragments.count()
        );
        while let Some(len) = fragments.next_frame(&mut self.chunk) {
            if let Err(error) = self.transport.send(&self.chunk[..len]).await {
                warn!(target: "control.ble", "Send failed (err={})", error);
                self.hub
                    .transport_error(Transport::Ble, error_kind::TRANSPORT);
                self.drop_request();
                return;
            }
            trace!(target: "control.ble", "Frame sent (size_b={})", len);
        }
    }
}

const _: () = assert!(MAX_REQUEST_LEN <= u16::MAX as usize);
