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
//! Request/response exchange over a chunked transport.

use std::time::Duration;

use rohi_control_ble::frame::{Fragmenter, Reassembler};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::limits::{MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};
use tokio::time::timeout;

use crate::error::{Error, Result};
use crate::model::ErrorCode;

/// Default time to wait for a response.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// A link to a device that moves chunks of at most
/// [`ClientTransport::max_payload`] bytes in both directions.
///
/// Implemented over BLE by [`BleLink`](crate::BleLink); tests implement it in
/// memory.
#[allow(async_fn_in_trait)]
pub trait ClientTransport {
    /// Largest chunk the link carries.
    fn max_payload(&self) -> usize;

    /// Writes one chunk to the device.
    async fn send(&mut self, chunk: &[u8]) -> Result<()>;

    /// Waits for the next chunk notified by the device.
    async fn recv(&mut self) -> Result<Vec<u8>>;
}

/// One connection to a device: frames requests, matches responses.
///
/// One request at a time, as the device allows only one in flight.
pub struct Session<T> {
    transport: T,
    next_request: u32,
    next_message: u8,
    timeout: Duration,
}

impl<T: ClientTransport> Session<T> {
    /// A session over `transport`.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_request: 1,
            next_message: 0,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Changes the response timeout of following calls.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Response timeout of calls.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Ends the session and returns the transport, for example to close it.
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Sends `call` and returns the matching response.
    ///
    /// A typed error response of the device becomes [`Error::Device`].
    pub async fn call(&mut self, call: Call) -> Result<Reply> {
        let limit = self.timeout;
        timeout(limit, self.exchange(call))
            .await
            .map_err(|_| Error::Timeout)?
    }

    async fn exchange(&mut self, call: Call) -> Result<Reply> {
        let request_id = self.next_request;
        self.next_request = self.next_request.wrapping_add(1).max(1);
        let message_id = self.next_message;
        self.next_message = self.next_message.wrapping_add(1);

        let mut wire = [0u8; MAX_REQUEST_LEN];
        let len = encode(
            &pb::Request {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                request: Some(call),
            },
            &mut wire,
        )?;

        let max_payload = self.transport.max_payload();
        let mut fragments = Fragmenter::new(message_id, &wire[..len], max_payload)?;
        let mut chunk = vec![0u8; max_payload];
        while let Some(len) = fragments.next_frame(&mut chunk) {
            self.transport.send(&chunk[..len]).await?;
        }
        wire.fill(0);

        let mut response = Reassembler::<MAX_RESPONSE_LEN>::new();
        loop {
            let frame = self.transport.recv().await?;
            // Tail frames of an abandoned response are not errors of this call.
            let Ok(Some(message)) = response.push(&frame) else {
                continue;
            };
            if message.id != message_id {
                // Late response of an earlier, abandoned request.
                continue;
            }
            let response: pb::Response = decode(message.bytes)?;
            if response.protocol_version != PROTOCOL_VERSION {
                return Err(Error::Protocol(format!(
                    "device speaks protocol version {}, expected {}",
                    response.protocol_version, PROTOCOL_VERSION
                )));
            }
            // Id 0: the device could not even parse the request.
            if response.request_id != request_id && response.request_id != 0 {
                continue;
            }
            return match response.response {
                Some(Reply::Error(error)) => Err(match error.reason() {
                    Some(reason) if error.code == pb::ErrorCode::Rejected => {
                        Error::Rejected(reason.as_str().to_owned())
                    }
                    _ => Error::Device(ErrorCode::from_proto(error.code)),
                }),
                Some(reply) => Ok(reply),
                None => Err(Error::Protocol("empty response".into())),
            };
        }
    }
}
