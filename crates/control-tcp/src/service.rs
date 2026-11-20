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
use core::time::Duration;

use embedded_io_async::{ErrorKind, ErrorType, Read, ReadExactError, Write};
use log::{debug, info, trace, warn};
use rohi_control::{Busy, Connection, ControlAccess, ControlHub, Transport};
use rohi_control_proto::control::{self as pb, Response_::Response as Reply};
use rohi_control_proto::limits::MAX_REQUEST_LEN;
use rohi_control_proto::{PROTOCOL_VERSION, encode};

use zeroize::Zeroizing;

use crate::frame::{HEADER_LEN, PLAIN_MAGIC, PLAIN_VERSION, decode_header, encode_header};
use crate::secure::{
    CLIENT_FINISH_LEN, CLIENT_HELLO_LEN, Channel, HandshakeError, MAGIC, NONCE_LEN, PSK_LEN,
    RECORD_HEADER_LEN, ServerHandshake, TAG_LEN,
};

/// The kinds of error the frontend reports to the endpoint status.
pub mod error_kind {
    /// A record header was rejected.
    pub const FRAME: u8 = 1;
    /// The connection ended inside a record or I/O failed, or a client was too slow to finish a
    /// handshake or a record in time.
    pub const IO: u8 = 2;
    /// The hub had no slot or no session for the request.
    pub const BUSY: u8 = 3;
    /// A client did not authenticate: a handshake with another key, or a record that did not verify.
    pub const AUTH: u8 = 4;
    /// A client asked for the handshake and the endpoint has no pre-shared key (or no random
    /// source).
    pub const NO_PSK: u8 = 5;
}

/// How long a client has, from the moment it is accepted, to send its preamble and, if it asks for
/// the handshake, to finish it. The limit is absolute: bytes that trickle in do not extend it.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a request record has, from its first byte to its last (header, data and tag), and how
/// long a response has to be written. The limit is absolute: a client that sends one byte at a
/// time does not extend it.
pub const RECORD_TIMEOUT: Duration = Duration::from_secs(10);

/// A client connection whose time can be bounded.
///
/// The serving loop asks for an absolute limit, not for a limit on each read: a client that sends
/// one byte just before every limit must not keep the connection (only one is served at a time).
pub trait Timed: Read + Write {
    /// Bounds the connection: with `Some(limit)`, every read and write that is still waiting
    /// `limit` after this call fails with an error of kind [`ErrorKind::TimedOut`], however many
    /// bytes arrived meanwhile. It replaces an earlier limit; `None` removes it, so a read waits
    /// for the client (a stream can still end an idle connection by itself).
    fn set_deadline(&mut self, limit: Option<Duration>);
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
    type Stream: Timed;

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
    /// The current key, `None` when none is provisioned: a client of the handshake is then refused.
    fn psk(&self) -> Option<[u8; PSK_LEN]>;
}

/// No key: a client of the handshake is refused; plaintext clients are served as the mode allows.
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
/// Every connection starts with a preamble that names its framing; nothing is guessed from other
/// bytes, and an unknown preamble ends the connection (counted as one error).
///
/// * **Handshake** (preamble of [`secure`](crate::secure), served in every mode): the client must
///   complete the handshake with the pre-shared key from the [`PskSource`] before a single request
///   byte is read or processed; a failed handshake, a record that does not authenticate, a
///   replayed or reordered record, a counter that is exhausted and a key that was replaced all end
///   the connection, with nothing written in plaintext and no fallback to plaintext. After the
///   handshake the link is reported to the policy as encrypted and authenticated, which is what
///   unlocks every request in every mode. Without a key, or without a random source, such a client
///   is refused.
/// * **Plaintext** (preamble of [`frame`](crate::frame), served in `Open` and `Protected`, refused
///   in `Private` before anything else is read): unauthenticated and unencrypted records. The link
///   keeps the default security (neither), so the policy of the mode decides what a request may
///   do: `Protected` lets reads through and refuses the rest, `Open` allows everything but a
///   `secure_link` action. For a loopback development setup or reads only.
///
/// The only connection is not held by a client that is slow rather than silent: the handshake and
/// every record have an absolute time limit ([`HANDSHAKE_TIMEOUT`], [`RECORD_TIMEOUT`], changed
/// with [`with_timeouts`](ControlTcp::with_timeouts)) that the arrival of single bytes does not
/// extend, and a response that cannot be written within [`RECORD_TIMEOUT`] ends the connection
/// too. Waiting for the first byte of a record is not limited here: the stream ends an idle
/// connection by itself (the host acceptor gives up after 60 seconds of silence).
///
/// One connection and one request in flight at a time, like a BLE central. Every connection is
/// its own [`session`](rohi_control::LinkSecurity::session) of the hub, unique across every
/// transport, so it cannot use the streams of another; the worker closes only that session's
/// streams when it ends.
pub struct ControlTcp<'h, 'a, A, K, const SLOTS: usize> {
    acceptor: A,
    hub: &'h ControlHub<'a, SLOTS>,
    key: K,
    handshake_timeout: Duration,
    record_timeout: Duration,
}

impl<'h, 'a, A, const SLOTS: usize> ControlTcp<'h, 'a, A, (), SLOTS>
where
    A: Acceptor,
{
    /// Serves the clients of `acceptor` through `hub`, without a pre-shared key: only the plaintext
    /// framing is served, as far as the access mode allows it.
    pub const fn new(acceptor: A, hub: &'h ControlHub<'a, SLOTS>) -> Self {
        Self {
            acceptor,
            hub,
            key: (),
            handshake_timeout: HANDSHAKE_TIMEOUT,
            record_timeout: RECORD_TIMEOUT,
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
        Self {
            acceptor,
            hub,
            key,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            record_timeout: RECORD_TIMEOUT,
        }
    }

    /// Replaces the absolute time limits of the handshake and of a record, see
    /// [`HANDSHAKE_TIMEOUT`] and [`RECORD_TIMEOUT`].
    #[must_use]
    pub const fn with_timeouts(mut self, handshake: Duration, record: Duration) -> Self {
        self.handshake_timeout = handshake;
        self.record_timeout = record;
        self
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
        self.serve(stream, &connection).await;
        connection.close().await;
        info!(target: "control.tcp", "Client disconnected (session={})", session);
        Ok(())
    }

    // The first bytes of the connection say how it goes on: the preamble of the handshake (`ROHI`)
    // or of plaintext records (`ROHP`). Nothing else is served, and nothing is guessed from other
    // bytes. The whole preamble is under the absolute handshake limit.
    async fn serve(&self, mut stream: A::Stream, connection: &Connection<'h, 'a, SLOTS>) {
        // Without an access state there is no mode and no random source: fail closed.
        let Some(access) = self.hub.access() else {
            return;
        };
        stream.set_deadline(Some(self.handshake_timeout));
        let mut magic = [0u8; 4];
        if let Err(error) = stream.read_exact(&mut magic).await {
            self.read_failed("the preamble", error);
            return;
        }
        if magic == MAGIC {
            self.serve_secure(stream, connection, magic).await;
        } else if magic == PLAIN_MAGIC {
            if !access.mode(Transport::Tcp).serves_plaintext() {
                // A policy refusal, not a failure of the transport: no downgrade, nothing read.
                self.refused_plaintext();
                return;
            }
            let mut version = [0u8; 1];
            if let Err(error) = stream.read_exact(&mut version).await {
                self.read_failed("the preamble", error);
                return;
            }
            if version[0] != PLAIN_VERSION {
                self.bad_preamble(version[0]);
                return;
            }
            self.serve_plain(stream, connection).await;
        } else {
            self.bad_preamble(magic[0]);
        }
    }

    #[cold]
    #[inline(never)]
    fn refused_plaintext(&self) {
        warn!(target: "control.tcp", "Plaintext client refused (mode=private)");
    }

    #[cold]
    #[inline(never)]
    fn bad_preamble(&self, first: u8) {
        warn!(target: "control.tcp", "Unknown preamble, client refused (first_byte={})", first);
        self.error(error_kind::FRAME);
    }

    // The handshake. `None` ends the connection; nothing was processed.
    async fn handshake(&self, stream: &mut A::Stream, magic: [u8; 4]) -> Option<(Channel, u32)> {
        // Without an access state there is no mode and no random source: fail closed.
        let access = self.hub.access()?;
        // The epoch before the key: a key replaced in between is noticed at the first record.
        let epoch = access.tcp_psk_epoch();
        // Wiped when dropped, so also when the handshake is cancelled.
        let Some(psk) = self.key.psk().map(Zeroizing::new) else {
            warn!(target: "control.tcp", "No pre-shared key is provisioned, client refused");
            self.error(error_kind::NO_PSK);
            return None;
        };
        let result = self.exchange(access, stream, magic, &psk).await;
        result.map(|channel| (channel, epoch))
    }

    async fn exchange(
        &self,
        access: &ControlAccess,
        stream: &mut A::Stream,
        magic: [u8; 4],
        psk: &[u8; PSK_LEN],
    ) -> Option<Channel> {
        let mut hello = [0u8; CLIENT_HELLO_LEN];
        hello[..magic.len()].copy_from_slice(&magic);
        if let Err(error) = stream.read_exact(&mut hello[magic.len()..]).await {
            self.read_failed("the handshake", error);
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
                self.failed_handshake(error);
                return None;
            }
        };
        let sent = async {
            stream.write_all(&server_hello).await?;
            stream.flush().await
        };
        if let Err(error) = sent.await {
            self.failed("the handshake", &error);
            return None;
        }
        let mut finish = [0u8; CLIENT_FINISH_LEN];
        if let Err(error) = stream.read_exact(&mut finish).await {
            self.read_failed("the handshake", error);
            return None;
        }
        match handshake.finish(&finish) {
            Ok(channel) => Some(channel),
            Err(error) => {
                self.failed_handshake(error);
                None
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn failed_handshake(&self, error: HandshakeError) {
        warn!(target: "control.tcp", "Client did not authenticate (err={:?})", error);
        self.error(error_kind::AUTH);
    }

    async fn serve_secure(
        &self,
        mut stream: A::Stream,
        connection: &Connection<'h, 'a, SLOTS>,
        magic: [u8; 4],
    ) {
        let Some((mut channel, epoch)) = self.handshake(&mut stream, magic).await else {
            return;
        };
        let Some(access) = self.hub.access() else {
            return;
        };
        // Only now is this the connection of an authenticated client.
        connection.set_link(true, true);
        connection.set_epoch(epoch);
        debug!(target: "control.tcp", "Client authenticated");
        loop {
            stream.set_deadline(None);
            // A key that was set, replaced or removed ends the connections of the old one.
            if access.tcp_psk_epoch() != epoch {
                info!(target: "control.tcp", "The key changed, closing the connection");
                return;
            }
            let mut header = [0u8; RECORD_HEADER_LEN];
            match self.read_header(&mut stream, &mut header).await {
                Ok(true) => {}
                Ok(false) => {
                    debug!(target: "control.tcp", "Client closed the connection");
                    return;
                }
                Err(()) => return,
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
                    // The record is read but never opened, so the receive counter would fall behind
                    // the client: answer once and end the connection; the client reconnects.
                    let mut tag = [0u8; TAG_LEN];
                    if !self.discard(&mut stream, len).await
                        || self.read_tag(&mut stream, &mut tag).await.is_err()
                    {
                        return;
                    }
                    self.send_secure_error(&mut stream, &mut channel, pb::ErrorCode::Busy)
                        .await;
                    return;
                }
            };
            let mut tag = [0u8; TAG_LEN];
            if let Err(error) = stream.read_exact(&mut reservation.buffer()[..len]).await {
                self.read_failed("a request", error);
                return;
            }
            if self.read_tag(&mut stream, &mut tag).await.is_err() {
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
        debug!(target: "control.tcp", "Plaintext connection");
        loop {
            stream.set_deadline(None);
            let mut header = [0u8; HEADER_LEN];
            match self.read_header(&mut stream, &mut header).await {
                Ok(true) => {}
                Ok(false) => {
                    debug!(target: "control.tcp", "Client closed the connection");
                    return;
                }
                Err(()) => return,
            }
            let len = match decode_header(header, MAX_REQUEST_LEN) {
                Ok(len) => len,
                Err(error) => {
                    // The length cannot be trusted, so the stream cannot be
                    // resynchronized: answer once and close.
                    warn!(target: "control.tcp", "Record rejected (err={})", error);
                    self.error(error_kind::FRAME);
                    self.send_plain_error(&mut stream, pb::ErrorCode::InvalidArgument)
                        .await;
                    return;
                }
            };
            let mut reservation = match connection.request() {
                Ok(reservation) => reservation,
                Err(Busy::Busy | Busy::Closed) => {
                    warn!(target: "control.tcp", "No request slot, request refused (size_b={})", len);
                    self.error(error_kind::BUSY);
                    if !self.discard(&mut stream, len).await
                        || !self
                            .send_plain_error(&mut stream, pb::ErrorCode::Busy)
                            .await
                    {
                        return;
                    }
                    continue;
                }
            };
            if let Err(error) = stream.read_exact(&mut reservation.buffer()[..len]).await {
                self.read_failed("a request", error);
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

    // Counts a failed read or write. A timeout is a client that was too slow.
    fn failed(&self, what: &str, error: &impl embedded_io_async::Error) {
        self.error(error_kind::IO);
        if error.kind() == ErrorKind::TimedOut {
            self.slow(what);
        } else {
            debug!(target: "control.tcp", "Connection ended inside {} (err={:?})", what, error);
        }
    }

    fn read_failed(&self, what: &str, error: ReadExactError<<A::Stream as ErrorType>::Error>) {
        match error {
            ReadExactError::Other(error) => self.failed(what, &error),
            ReadExactError::UnexpectedEof => {
                self.error(error_kind::IO);
                debug!(target: "control.tcp", "Connection ended inside {}", what);
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn slow(&self, what: &str) {
        warn!(target: "control.tcp", "Client too slow, closing the connection (in={})", what);
    }

    // A record header. The wait for its first byte is not limited; the limit of the whole record,
    // header, data and tag, starts with that byte. `Ok(false)` is a clean end of the stream before
    // the first byte; `Err` ended the connection (counted).
    async fn read_header<const N: usize>(
        &self,
        stream: &mut A::Stream,
        header: &mut [u8; N],
    ) -> Result<bool, ()> {
        match stream.read(&mut header[..1]).await {
            Ok(0) => return Ok(false),
            Ok(_) => {}
            Err(error) => {
                self.failed("a record header", &error);
                return Err(());
            }
        }
        stream.set_deadline(Some(self.record_timeout));
        match stream.read_exact(&mut header[1..]).await {
            Ok(()) => Ok(true),
            Err(error) => {
                self.read_failed("a record header", error);
                Err(())
            }
        }
    }

    async fn read_tag(&self, stream: &mut A::Stream, tag: &mut [u8; TAG_LEN]) -> Result<(), ()> {
        match stream.read_exact(tag).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.read_failed("a request", error);
                Err(())
            }
        }
    }

    // Reads and drops `len` bytes; `false` if the connection ended.
    async fn discard(&self, stream: &mut A::Stream, mut len: usize) -> bool {
        let mut scratch = [0u8; 64];
        while len > 0 {
            let take = len.min(scratch.len());
            if let Err(error) = stream.read_exact(&mut scratch[..take]).await {
                self.read_failed("a record", error);
                return false;
            }
            len -= take;
        }
        true
    }

    // One encrypted record: header, data (already sealed in place) and tag.
    async fn write_record(
        &self,
        stream: &mut A::Stream,
        header: [u8; RECORD_HEADER_LEN],
        sealed: &[u8],
        tag: [u8; TAG_LEN],
    ) -> bool {
        stream.set_deadline(Some(self.record_timeout));
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
                self.failed("a response", &error);
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
        stream.set_deadline(Some(self.record_timeout));
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
                self.failed("a response", &error);
                false
            }
        }
    }

    /// Answers a plaintext client with a typed error that has no request id.
    async fn send_plain_error(&self, stream: &mut A::Stream, code: pb::ErrorCode) -> bool {
        let mut bytes = [0u8; 16];
        let Some(written) = encode_error(code, &mut bytes) else {
            return false;
        };
        self.send(stream, &bytes[..written]).await
    }
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
