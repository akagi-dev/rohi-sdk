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

use core::fmt::Debug;

use embedded_io_async::{Read, Write};
use log::{debug, info, trace, warn};
use rohi_control::{AuthorizationPolicy, ControlBackend, Controller, LinkSecurity};
use rohi_control_proto::control::{self as pb, Response_::Response as Reply};
use rohi_control_proto::limits::{MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use rohi_control_proto::{PROTOCOL_VERSION, encode};

use crate::frame::{HEADER_LEN, decode_header, encode_header};

/// A source of client connections, usually a listening socket.
///
/// Implemented by platform crates (`rohi-net-std` has a TCP listener). Only
/// one connection is served at a time, so a platform may leave further clients
/// in its backlog until the current one ends.
///
/// Cancellation: `accept` must be cancel safe; dropping it must not lose a
/// connection that was not returned.
#[allow(async_fn_in_trait)]
pub trait Acceptor {
    /// A connection: an ordered byte stream to one client. Dropping it closes
    /// the connection.
    type Stream: Read + Write;

    /// A failure that makes the acceptor unusable. Transient failures
    /// (for example a client that gave up before it was accepted) are handled
    /// inside the implementation.
    type Error: Debug;

    /// Waits for the next client.
    async fn accept(&mut self) -> Result<Self::Stream, Self::Error>;
}

/// Binds a [`Controller`] to an [`Acceptor`].
///
/// ```text
/// acceptor.accept -> loop { read record -> Controller::handle -> write record }
/// ```
///
/// One connection and one request in flight at a time, like a BLE central. The
/// buffers are inline and fixed (about 3 KiB: [`MAX_REQUEST_LEN`] for the
/// request and [`MAX_RESPONSE_LEN`] for the response, plus its header); place
/// the value in a `static` or in the future of the control task.
///
/// The authorization of requests follows the [`LinkSecurity`] of the link, which
/// is always unencrypted and unauthenticated: this transport has no pairing or
/// encryption, see the [crate documentation](crate). Every connection is its
/// own [`session`](LinkSecurity::session), so it cannot use the streams of
/// another, and [`Controller::link_lost`] runs when it ends.
pub struct ControlTcp<'c, A, S, P> {
    acceptor: A,
    controller: &'c Controller<S, P>,
    sessions: u32,
    request: [u8; MAX_REQUEST_LEN],
    response: [u8; HEADER_LEN + MAX_RESPONSE_LEN],
}

impl<'c, A, S, P> ControlTcp<'c, A, S, P>
where
    A: Acceptor,
    S: ControlBackend,
    P: AuthorizationPolicy,
{
    /// Serves `controller` to the clients of `acceptor`.
    pub const fn new(acceptor: A, controller: &'c Controller<S, P>) -> Self {
        Self {
            acceptor,
            controller,
            sessions: 0,
            request: [0; MAX_REQUEST_LEN],
            response: [0; HEADER_LEN + MAX_RESPONSE_LEN],
        }
    }

    /// The wrapped acceptor.
    pub fn acceptor(&self) -> &A {
        &self.acceptor
    }

    /// Accepts clients one after the other until the acceptor fails, and
    /// returns that error.
    ///
    /// A problem with one connection (malformed record, disconnect, I/O
    /// error) ends that connection only; the next one starts from a clean
    /// state. Cancel it by dropping the future: the connection being served is
    /// closed.
    pub async fn run(&mut self) -> A::Error {
        loop {
            if let Err(error) = self.serve_next().await {
                return error;
            }
        }
    }

    /// Waits for one client and serves it until the connection ends.
    ///
    /// Cancellation: dropping the future closes the connection and abandons
    /// the request being handled (no response is sent for it). Operations the
    /// services already committed stay committed.
    ///
    /// # Errors
    ///
    /// The error of [`Acceptor::accept`].
    pub async fn serve_next(&mut self) -> Result<(), A::Error> {
        let stream = match self.acceptor.accept().await {
            Ok(stream) => stream,
            Err(error) => {
                log::error!(target: "control.tcp", "Acceptor failed, serving stops (err={:?})", error);
                return Err(error);
            }
        };
        self.sessions = self.sessions.wrapping_add(1);
        info!(target: "control.tcp", "Client connected (session={})", self.sessions);
        self.serve(stream).await;
        self.controller.link_lost();
        info!(target: "control.tcp", "Client disconnected (session={})", self.sessions);
        Ok(())
    }

    async fn serve(&mut self, mut stream: A::Stream) {
        let link = LinkSecurity {
            session: self.sessions,
            ..LinkSecurity::default()
        };
        loop {
            let mut header = [0u8; HEADER_LEN];
            match read_header(&mut stream, &mut header).await {
                Ok(true) => {}
                Ok(false) => {
                    debug!(target: "control.tcp", "Client closed the connection");
                    return;
                }
                Err(()) => {
                    debug!(target: "control.tcp", "Connection ended inside a record header");
                    return;
                }
            }
            let len = match decode_header(header, MAX_REQUEST_LEN) {
                Ok(len) => len,
                Err(error) => {
                    // The length cannot be trusted, so the stream cannot be
                    // resynchronized: answer once and close.
                    warn!(target: "control.tcp", "Record rejected (err={})", error);
                    self.send_error(&mut stream).await;
                    return;
                }
            };
            if let Err(error) = stream.read_exact(&mut self.request[..len]).await {
                debug!(
                    target: "control.tcp",
                    "Connection ended inside a request (size_b={} err={:?})",
                    len, error
                );
                return;
            }
            trace!(target: "control.tcp", "Request record received (size_b={})", len);
            let written = self
                .controller
                .handle(
                    &link,
                    &self.request[..len],
                    &mut self.response[HEADER_LEN..],
                )
                .await;
            if written == 0 {
                warn!(target: "control.tcp", "No response to send, closing the connection");
                return;
            }
            if !self.send(&mut stream, written).await {
                return;
            }
        }
    }

    async fn send_error(&mut self, stream: &mut A::Stream) {
        let response = pb::Response {
            protocol_version: PROTOCOL_VERSION,
            request_id: 0,
            response: Some(Reply::Error(pb::Error {
                code: pb::ErrorCode::InvalidArgument,
                ..Default::default()
            })),
        };
        match encode(&response, &mut self.response[HEADER_LEN..]) {
            Ok(written) => {
                self.send(stream, written).await;
            }
            Err(error) => {
                warn!(target: "control.tcp", "Cannot encode the error response (err={:?})", error)
            }
        }
    }

    /// Writes the record whose payload is `response[HEADER_LEN..][..written]`.
    async fn send(&mut self, stream: &mut A::Stream, written: usize) -> bool {
        let Ok(len) = u32::try_from(written) else {
            warn!(
                target: "control.tcp",
                "Response cannot be framed (size_b={})",
                written
            );
            return false;
        };
        self.response[..HEADER_LEN].copy_from_slice(&encode_header(len));
        let result = async {
            stream
                .write_all(&self.response[..HEADER_LEN + written])
                .await?;
            stream.flush().await
        }
        .await;
        match result {
            Ok(()) => {
                trace!(target: "control.tcp", "Response record sent (size_b={})", written);
                true
            }
            Err(error) => {
                debug!(
                    target: "control.tcp",
                    "Sending the response failed (size_b={} err={:?})",
                    written, error
                );
                false
            }
        }
    }
}

/// Reads a record header. `Ok(false)` is a clean end of the stream before the
/// first byte; an end inside the header is an error.
async fn read_header<R: Read>(stream: &mut R, header: &mut [u8; HEADER_LEN]) -> Result<bool, ()> {
    match stream.read(&mut header[..1]).await {
        Ok(0) => return Ok(false),
        Ok(_) => {}
        Err(error) => {
            debug!(target: "control.tcp", "Reading a record header failed (err={:?})", error);
            return Err(());
        }
    }
    stream.read_exact(&mut header[1..]).await.map_err(|error| {
        debug!(target: "control.tcp", "Reading a record header failed (err={:?})", error);
    })?;
    Ok(true)
}
