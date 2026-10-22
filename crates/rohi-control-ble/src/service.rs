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
use log::{debug, warn};
use rohi_control::{AuthorizationPolicy, ControlServices, Controller, LinkSecurity};
use rohi_control_proto::control::{self as pb, Response_::Response as Reply};
use rohi_control_proto::limits::{MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use rohi_control_proto::{PROTOCOL_VERSION, encode};
use rohi_net::{BleError, BleTransport, MAX_BLE_PAYLOAD};

use crate::frame::{Fragmenter, Reassembler};

/// Binds a [`Controller`] to a [`BleTransport`].
///
/// ```text
/// transport.recv -> Reassembler -> Controller::handle -> Fragmenter -> transport.send
/// ```
///
/// One connection, one request in flight, one response. All buffers are
/// inline and fixed; place the value in a `static` or in the future of the
/// control task. Nothing here runs from a radio callback: the transport
/// queues chunks and this loop does the work.
pub struct ControlBle<'c, T, S, P> {
    transport: T,
    controller: &'c Controller<S, P>,
    request: Reassembler<MAX_REQUEST_LEN>,
    response: [u8; MAX_RESPONSE_LEN],
    chunk: [u8; MAX_BLE_PAYLOAD],
}

impl<'c, T, S, P> ControlBle<'c, T, S, P>
where
    T: BleTransport,
    S: ControlServices,
    P: AuthorizationPolicy,
{
    /// Serves `controller` over `transport`.
    pub const fn new(transport: T, controller: &'c Controller<S, P>) -> Self {
        Self {
            transport,
            controller,
            request: Reassembler::new(),
            response: [0; MAX_RESPONSE_LEN],
            chunk: [0; MAX_BLE_PAYLOAD],
        }
    }

    /// The wrapped transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Serves requests forever. Failures end the current exchange only;
    /// the next connection starts from a clean state.
    pub async fn run(&mut self) -> ! {
        loop {
            self.step().await;
        }
    }

    /// Waits for one transport event and handles it: a frame, which may
    /// complete a request and send its response, or a disconnect.
    pub async fn step(&mut self) {
        let received = self.transport.recv(&mut self.chunk).await;
        let len = match received {
            Ok(len) => len,
            Err(BleError::Disconnected) => {
                debug!("control: central disconnected");
                self.request.reset();
                return;
            }
            Err(error) => {
                warn!("control: BLE receive failed: {}", error);
                self.request.reset();
                return;
            }
        };

        let epoch = self.transport.connection_epoch();
        let link = self
            .transport
            .connection_info()
            .map(LinkSecurity::from)
            .unwrap_or_default();

        let outcome = match self.request.push(&self.chunk[..len]) {
            Ok(None) => return,
            Ok(Some(message)) => {
                let id = message.id;
                let written = self
                    .controller
                    .handle(&link, message.bytes, &mut self.response)
                    .await;
                Ok((id, written))
            }
            Err(error) => Err(error),
        };

        match outcome {
            Ok((id, written)) => {
                self.request.reset();
                if written > 0 {
                    self.send_response(id, written, epoch).await;
                }
            }
            Err(error) => {
                warn!("control: rejected frame: {}", error);
                let id = self.request.last_id();
                self.send_error(id, epoch).await;
            }
        }
    }

    async fn send_error(&mut self, id: u8, epoch: u32) {
        let response = pb::Response {
            protocol_version: PROTOCOL_VERSION,
            request_id: 0,
            response: Some(Reply::Error(pb::Error {
                code: pb::ErrorCode::InvalidArgument,
            })),
        };
        if let Ok(written) = encode(&response, &mut self.response) {
            self.send_response(id, written, epoch).await;
        }
    }

    /// Sends `response[..written]` unless the connection changed meanwhile.
    async fn send_response(&mut self, id: u8, written: usize, epoch: u32) {
        if self.transport.connection_info().is_none() || self.transport.connection_epoch() != epoch
        {
            debug!("control: link changed, response dropped");
            self.request.reset();
            return;
        }
        let max = self.transport.max_payload().min(MAX_BLE_PAYLOAD);
        let mut fragments = match Fragmenter::new(id, &self.response[..written], max) {
            Ok(fragments) => fragments,
            Err(error) => {
                warn!("control: cannot frame the response: {}", error);
                return;
            }
        };
        while let Some(len) = fragments.next_frame(&mut self.chunk) {
            if let Err(error) = self.transport.send(&self.chunk[..len]).await {
                warn!("control: BLE send failed: {}", error);
                self.request.reset();
                return;
            }
        }
    }
}
