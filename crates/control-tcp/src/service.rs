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
use rohi_control::{Busy, Connection, ControlHub, Transport};
use rohi_control_proto::control::{self as pb, Response_::Response as Reply};
use rohi_control_proto::limits::MAX_REQUEST_LEN;
use rohi_control_proto::{PROTOCOL_VERSION, encode};

use crate::frame::{HEADER_LEN, decode_header, encode_header};

/// The kinds of error the frontend reports to the endpoint status.
pub mod error_kind {
    /// A record header was rejected.
    pub const FRAME: u8 = 1;
    /// The connection ended inside a record or I/O failed.
    pub const IO: u8 = 2;
    /// The hub had no slot or no session for the request.
    pub const BUSY: u8 = 3;
}

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

/// The TCP frontend of the control worker.
///
/// ```text
/// acceptor.accept -> loop { read record into a hub slot ==> control_task (Controller)
///                           write record from the same slot <=====+ }
/// ```
///
/// It owns the connection and the record framing and nothing else: it never
/// calls the [`Controller`](rohi_control::Controller). A request record is read
/// straight into a request slot of the [`ControlHub`], handed to the one
/// worker ([`ControlHub::serve`]) and the response record is written from the
/// same slot, so the frontend holds no message buffers of its own.
///
/// One connection and one request in flight at a time, like a BLE central.
/// Every connection is its own [`session`](rohi_control::LinkSecurity::session)
/// of the hub, unique across every transport, so it cannot use the streams or
/// the challenge of another; the worker closes only that session's streams
/// when it ends. The connection is [`Transport::Tcp`], whose access mode
/// decides what an unsigned request may do; the link itself is always
/// unencrypted and unauthenticated, see the [crate documentation](crate).
pub struct ControlTcp<'h, 'a, A, const SLOTS: usize> {
    acceptor: A,
    hub: &'h ControlHub<'a, SLOTS>,
}

impl<'h, 'a, A, const SLOTS: usize> ControlTcp<'h, 'a, A, SLOTS>
where
    A: Acceptor,
{
    /// Serves the clients of `acceptor` through `hub`.
    pub const fn new(acceptor: A, hub: &'h ControlHub<'a, SLOTS>) -> Self {
        Self { acceptor, hub }
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
        let connection = match self.hub.connect(Transport::Tcp) {
            Ok(connection) => connection,
            Err(_) => {
                warn!(target: "control.tcp", "Too many connections, client refused");
                self.hub.transport_error(Transport::Tcp, error_kind::BUSY);
                return Ok(());
            }
        };
        let session = connection.id().get();
        info!(target: "control.tcp", "Client connected (session={})", session);
        self.serve(stream, &connection).await;
        connection.close().await;
        info!(target: "control.tcp", "Client disconnected (session={})", session);
        Ok(())
    }

    async fn serve(&self, mut stream: A::Stream, connection: &Connection<'h, 'a, SLOTS>) {
        loop {
            let mut header = [0u8; HEADER_LEN];
            match read_header(&mut stream, &mut header).await {
                Ok(true) => {}
                Ok(false) => {
                    debug!(target: "control.tcp", "Client closed the connection");
                    return;
                }
                Err(()) => {
                    self.error(error_kind::IO);
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
                    self.error(error_kind::FRAME);
                    send_error(&mut stream, pb::ErrorCode::InvalidArgument).await;
                    return;
                }
            };
            let mut reservation = match connection.request() {
                Ok(reservation) => reservation,
                Err(Busy::Busy | Busy::Closed) => {
                    warn!(target: "control.tcp", "No request slot, request refused (size_b={})", len);
                    self.error(error_kind::BUSY);
                    if !discard(&mut stream, len).await
                        || !send_error(&mut stream, pb::ErrorCode::Busy).await
                    {
                        return;
                    }
                    continue;
                }
            };
            if let Err(error) = stream.read_exact(&mut reservation.buffer()[..len]).await {
                self.error(error_kind::IO);
                debug!(
                    target: "control.tcp",
                    "Connection ended inside a request (size_b={} err={:?})",
                    len, error
                );
                return;
            }
            trace!(target: "control.tcp", "Request record received (size_b={})", len);
            let response = reservation.submit(len).await;
            if response.bytes().is_empty() {
                warn!(target: "control.tcp", "No response to send, closing the connection");
                return;
            }
            if !self.send(&mut stream, response.bytes()).await {
                return;
            }
        }
    }

    fn error(&self, kind: u8) {
        self.hub.transport_error(Transport::Tcp, kind);
    }

    /// Writes one record with `payload`.
    async fn send(&self, stream: &mut A::Stream, payload: &[u8]) -> bool {
        let Ok(len) = u32::try_from(payload.len()) else {
            warn!(
                target: "control.tcp",
                "Response cannot be framed (size_b={})",
                payload.len()
            );
            return false;
        };
        let result = async {
            stream.write_all(&encode_header(len)).await?;
            stream.write_all(payload).await?;
            stream.flush().await
        }
        .await;
        match result {
            Ok(()) => {
                trace!(target: "control.tcp", "Response record sent (size_b={})", payload.len());
                true
            }
            Err(error) => {
                self.error(error_kind::IO);
                debug!(
                    target: "control.tcp",
                    "Sending the response failed (size_b={} err={:?})",
                    payload.len(), error
                );
                false
            }
        }
    }
}

/// Reads and drops `len` bytes; `false` if the connection ended.
async fn discard<R: Read>(stream: &mut R, mut len: usize) -> bool {
    let mut scratch = [0u8; 64];
    while len > 0 {
        let take = len.min(scratch.len());
        if stream.read_exact(&mut scratch[..take]).await.is_err() {
            return false;
        }
        len -= take;
    }
    true
}

/// Answers with a typed error that has no request id.
async fn send_error<W: Write>(stream: &mut W, code: pb::ErrorCode) -> bool {
    let response = pb::Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: 0,
        response: Some(Reply::Error(pb::Error {
            code,
            ..Default::default()
        })),
    };
    let mut bytes = [0u8; 16];
    let Ok(written) = encode(&response, &mut bytes) else {
        warn!(target: "control.tcp", "Cannot encode the error response");
        return false;
    };
    let Ok(len) = u32::try_from(written) else {
        return false;
    };
    let result = async {
        stream.write_all(&encode_header(len)).await?;
        stream.write_all(&bytes[..written]).await?;
        stream.flush().await
    }
    .await;
    result.is_ok()
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
