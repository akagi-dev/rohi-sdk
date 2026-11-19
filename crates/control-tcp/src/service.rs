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
use rohi_control::{Busy, Connection, ControlAccess, ControlHub, Transport, TransportAccessMode};
use rohi_control_proto::control::{self as pb, Response_::Response as Reply};
use rohi_control_proto::limits::MAX_REQUEST_LEN;
use rohi_control_proto::{PROTOCOL_VERSION, encode};

use zeroize::Zeroize;

use crate::frame::{HEADER_LEN, decode_header, encode_header};
use crate::secure::{
    CLIENT_FINISH_LEN, CLIENT_HELLO_LEN, Channel, HandshakeError, NONCE_LEN, PSK_LEN,
    RECORD_HEADER_LEN, ServerHandshake, TAG_LEN,
};

/// The kinds of error the frontend reports to the endpoint status.
pub mod error_kind {
    /// A record header was rejected.
    pub const FRAME: u8 = 1;
    /// The connection ended inside a record or I/O failed.
    pub const IO: u8 = 2;
    /// The hub had no slot or no session for the request.
    pub const BUSY: u8 = 3;
    /// A client did not authenticate: a handshake with another key, or a record that did not verify.
    pub const AUTH: u8 = 4;
    /// A restricted endpoint has no pre-shared key (or no random source) and refused a client.
    pub const NO_PSK: u8 = 5;
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

/// Where the pre-shared key of the endpoint comes from.
///
/// Asked once for every connection, so a key that was replaced applies to the next client; the
/// caller wipes its copy after the handshake. The platform stores the key (the configuration
/// owner); this crate never keeps one.
pub trait PskSource {
    /// The current key, `None` when none is provisioned: a restricted endpoint then serves
    /// nobody.
    fn psk(&self) -> Option<[u8; PSK_LEN]>;
}

/// No key: a restricted endpoint serves nobody, an open one is plaintext.
impl PskSource for () {
    fn psk(&self) -> Option<[u8; PSK_LEN]> {
        None
    }
}

/// The TCP frontend of the control worker.
///
/// ```text
/// acceptor.accept -> [handshake with the pre-shared key] ->
///                    loop { read record into a hub slot ==> control_task (Controller)
///                           write record from the same slot <=====+ }
/// ```
///
/// It owns the connection, the handshake, the record framing and the encryption of the
/// records, and nothing else: it never calls the [`Controller`](rohi_control::Controller). A
/// request record is read straight into a request slot of the [`ControlHub`], decrypted where
/// it is, handed to the one worker ([`ControlHub::serve`]) and the response is encrypted in the
/// same slot and written from it, so the frontend holds no message buffers of its own.
///
/// What a client gets depends on the access mode of the TCP endpoint in the
/// [`ControlAccess`](rohi_control::ControlAccess) of the hub:
///
/// * **`Restricted`** (the default): the secure channel of [`secure`](crate::secure) is
///   mandatory. The client must complete the handshake with the pre-shared key from the
///   [`PskSource`] before a single request byte is read or processed; a failed handshake, a
///   record that does not authenticate, a replayed or reordered record, a counter that is
///   exhausted and a key that was replaced all end the connection, with nothing written in
///   plaintext. After the handshake the link is reported to the policy as encrypted and
///   authenticated. Without a key, or without a random source, nobody is served.
/// * **`Open`**: plaintext records (see [`frame`](crate::frame)), unauthenticated and
///   unencrypted: for a loopback development setup only. A request on it is not trusted by the
///   policy beyond what the open mode allows.
///
/// One connection and one request in flight at a time, like a BLE central. Every connection is
/// its own [`session`](rohi_control::LinkSecurity::session) of the hub, unique across every
/// transport, so it cannot use the streams of another; the worker closes only that session's
/// streams when it ends.
pub struct ControlTcp<'h, 'a, A, K, const SLOTS: usize> {
    acceptor: A,
    hub: &'h ControlHub<'a, SLOTS>,
    key: K,
}

impl<'h, 'a, A, const SLOTS: usize> ControlTcp<'h, 'a, A, (), SLOTS>
where
    A: Acceptor,
{
    /// Serves the clients of `acceptor` through `hub`, without a pre-shared key: only an endpoint
    /// in `Open` mode (plaintext) serves anybody.
    pub const fn new(acceptor: A, hub: &'h ControlHub<'a, SLOTS>) -> Self {
        Self {
            acceptor,
            hub,
            key: (),
        }
    }
}

impl<'h, 'a, A, K, const SLOTS: usize> ControlTcp<'h, 'a, A, K, SLOTS>
where
    A: Acceptor,
    K: PskSource,
{
    /// Serves the clients of `acceptor` through `hub`, authenticating them with the key of `key`.
    pub const fn with_psk(acceptor: A, hub: &'h ControlHub<'a, SLOTS>, key: K) -> Self {
        Self { acceptor, hub, key }
    }

    /// The wrapped acceptor.
    pub fn acceptor(&self) -> &A {
        &self.acceptor
    }

    /// Accepts clients one after the other until the acceptor fails, and
    /// returns that error.
    ///
    /// A problem with one connection (malformed record, failed authentication, disconnect, I/O
    /// error) ends that connection only; the next one starts from a clean
    /// state. Cancel it by dropping the future: the connection being served is
    /// closed and the keys of its channel are wiped.
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
        let open = self
            .hub
            .access()
            .is_some_and(|access| access.mode(Transport::Tcp) == TransportAccessMode::Open);
        if open {
            self.serve_plain(stream, &connection).await;
        } else {
            self.serve_secure(stream, &connection).await;
        }
        connection.close().await;
        info!(target: "control.tcp", "Client disconnected (session={})", session);
        Ok(())
    }

    // The handshake. `None` ends the connection; nothing was processed.
    async fn handshake(&self, stream: &mut A::Stream) -> Option<(Channel, u32)> {
        // Without an access state there is no mode and no random source: fail closed.
        let access = self.hub.access()?;
        // The epoch before the key: a key replaced in between is noticed at the first record.
        let epoch = access.tcp_psk_epoch();
        let Some(mut psk) = self.key.psk() else {
            warn!(target: "control.tcp", "No pre-shared key is provisioned, client refused");
            self.error(error_kind::NO_PSK);
            return None;
        };
        let result = self.exchange(access, stream, &psk).await;
        psk.zeroize();
        result.map(|channel| (channel, epoch))
    }

    async fn exchange(
        &self,
        access: &ControlAccess,
        stream: &mut A::Stream,
        psk: &[u8; PSK_LEN],
    ) -> Option<Channel> {
        let mut hello = [0u8; CLIENT_HELLO_LEN];
        if stream.read_exact(&mut hello).await.is_err() {
            self.error(error_kind::IO);
            debug!(target: "control.tcp", "Connection ended inside the handshake");
            return None;
        }
        let mut nonce = [0u8; NONCE_LEN];
        if !access.fill_random(&mut nonce) {
            warn!(target: "control.tcp", "No random source, client refused");
            self.error(error_kind::NO_PSK);
            return None;
        }
        let (handshake, server_hello) = match ServerHandshake::respond(psk, &hello, &nonce) {
            Ok(answer) => answer,
            Err(error) => {
                self.failed_handshake(access, error);
                return None;
            }
        };
        if stream.write_all(&server_hello).await.is_err() || stream.flush().await.is_err() {
            self.error(error_kind::IO);
            return None;
        }
        let mut finish = [0u8; CLIENT_FINISH_LEN];
        if stream.read_exact(&mut finish).await.is_err() {
            self.error(error_kind::IO);
            debug!(target: "control.tcp", "Connection ended inside the handshake");
            return None;
        }
        match handshake.finish(&finish) {
            Ok(channel) => Some(channel),
            Err(error) => {
                self.failed_handshake(access, error);
                None
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn failed_handshake(&self, access: &ControlAccess, error: HandshakeError) {
        warn!(target: "control.tcp", "Client did not authenticate (err={:?})", error);
        access.auth_failed(Transport::Tcp);
        self.error(error_kind::AUTH);
    }

    async fn serve_secure(&self, mut stream: A::Stream, connection: &Connection<'h, 'a, SLOTS>) {
        let Some((mut channel, epoch)) = self.handshake(&mut stream).await else {
            return;
        };
        let Some(access) = self.hub.access() else {
            return;
        };
        // Only now is this the connection of an authenticated client.
        connection.set_link(true, true);
        debug!(target: "control.tcp", "Client authenticated");
        loop {
            // A key that was set, replaced or removed ends the connections of the old one.
            if access.tcp_psk_epoch() != epoch {
                info!(target: "control.tcp", "The key changed, closing the connection");
                return;
            }
            let mut header = [0u8; RECORD_HEADER_LEN];
            match read_header(&mut stream, &mut header).await {
                Ok(true) => {}
                Ok(false) => {
                    debug!(target: "control.tcp", "Client closed the connection");
                    return;
                }
                Err(()) => {
                    self.error(error_kind::IO);
                    return;
                }
            }
            let len = match channel.open_header(header, MAX_REQUEST_LEN) {
                Ok(len) => len,
                Err(error) => {
                    // Nothing can be trusted about the stream any more: end it, in silence.
                    warn!(target: "control.tcp", "Record rejected (err={:?})", error);
                    self.error(error_kind::FRAME);
                    return;
                }
            };
            let mut reservation = match connection.request() {
                Ok(reservation) => reservation,
                Err(Busy::Busy | Busy::Closed) => {
                    warn!(target: "control.tcp", "No request slot, request refused (size_b={})", len);
                    self.error(error_kind::BUSY);
                    // The record is read and checked, so the counters stay in step.
                    let mut tag = [0u8; TAG_LEN];
                    if !discard(&mut stream, len).await
                        || stream.read_exact(&mut tag).await.is_err()
                    {
                        return;
                    }
                    if !self
                        .send_secure_error(&mut stream, &mut channel, pb::ErrorCode::Busy)
                        .await
                    {
                        return;
                    }
                    continue;
                }
            };
            let mut tag = [0u8; TAG_LEN];
            if stream
                .read_exact(&mut reservation.buffer()[..len])
                .await
                .is_err()
                || stream.read_exact(&mut tag).await.is_err()
            {
                self.error(error_kind::IO);
                debug!(target: "control.tcp", "Connection ended inside a request (size_b={})", len);
                return;
            }
            // A key that changed while the record was on its way revokes it too.
            if access.tcp_psk_epoch() != epoch {
                info!(target: "control.tcp", "The key changed, closing the connection");
                return;
            }
            if let Err(error) = channel.open(header, &mut reservation.buffer()[..len], &tag) {
                // Tampered, replayed, reordered or truncated: fail closed, no response.
                warn!(target: "control.tcp", "Record did not authenticate (err={:?})", error);
                access.auth_failed(Transport::Tcp);
                self.error(error_kind::AUTH);
                return;
            }
            trace!(target: "control.tcp", "Request record received (size_b={})", len);
            let mut response = reservation.submit(len).await;
            if response.bytes().is_empty() {
                warn!(target: "control.tcp", "No response to send, closing the connection");
                return;
            }
            let sealed = channel.seal(response.bytes_mut());
            let Ok((header, tag)) = sealed else {
                // The counter is used up: the response cannot be sent and none ever can.
                warn!(target: "control.tcp", "The record counter is exhausted, closing the connection");
                return;
            };
            if !self
                .write_record(&mut stream, header, response.bytes(), tag)
                .await
            {
                return;
            }
        }
    }

    async fn serve_plain(&self, mut stream: A::Stream, connection: &Connection<'h, 'a, SLOTS>) {
        warn!(target: "control.tcp", "Plaintext connection: the endpoint is open");
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

    // One encrypted record: header, data (already sealed in place) and tag.
    async fn write_record(
        &self,
        stream: &mut A::Stream,
        header: [u8; RECORD_HEADER_LEN],
        sealed: &[u8],
        tag: [u8; TAG_LEN],
    ) -> bool {
        let result = async {
            stream.write_all(&header).await?;
            stream.write_all(sealed).await?;
            stream.write_all(&tag).await?;
            stream.flush().await
        }
        .await;
        match result {
            Ok(()) => {
                trace!(target: "control.tcp", "Response record sent (size_b={})", sealed.len());
                true
            }
            Err(error) => {
                self.error(error_kind::IO);
                debug!(target: "control.tcp", "Sending the response failed (err={:?})", error);
                false
            }
        }
    }

    // A typed error with no request id, as an encrypted record.
    async fn send_secure_error(
        &self,
        stream: &mut A::Stream,
        channel: &mut Channel,
        code: pb::ErrorCode,
    ) -> bool {
        let mut bytes = [0u8; 16];
        let Some(written) = encode_error(code, &mut bytes) else {
            return false;
        };
        let Ok((header, tag)) = channel.seal(&mut bytes[..written]) else {
            return false;
        };
        self.write_record(stream, header, &bytes[..written], tag)
            .await
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

/// A typed error with no request id, encoded; `None` when it does not fit.
fn encode_error(code: pb::ErrorCode, bytes: &mut [u8; 16]) -> Option<usize> {
    let response = pb::Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: 0,
        response: Some(Reply::Error(pb::Error {
            code,
            ..Default::default()
        })),
    };
    let written = encode(&response, bytes);
    if written.is_err() {
        warn!(target: "control.tcp", "Cannot encode the error response");
    }
    written.ok()
}

/// Answers a plaintext client with a typed error that has no request id.
async fn send_error<W: Write>(stream: &mut W, code: pb::ErrorCode) -> bool {
    let mut bytes = [0u8; 16];
    let Some(written) = encode_error(code, &mut bytes) else {
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
async fn read_header<R: Read, const N: usize>(
    stream: &mut R,
    header: &mut [u8; N],
) -> Result<bool, ()> {
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
