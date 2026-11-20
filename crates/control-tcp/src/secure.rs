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

//! The secure channel of the TCP endpoint: a handshake with a 32-byte pre-shared key and an
//! authenticated, encrypted record layer. Everything here is plain functions over byte arrays,
//! with no I/O, so that the device ([`ControlTcp`](crate::ControlTcp)) and the `rohi` tool use
//! the same code.
//!
//! # Protocol (version 1)
//!
//! ```text
//! client                                                   device
//!   ClientHello  "ROHI" | version | client_nonce[32]   ──►
//!                                                      ◄──   ServerHello  "ROHI" | version | server_nonce[32] | server_proof[32]
//!   ClientFinish client_proof[32]                      ──►
//!   records ◄──────────────────────────────────────────────► records
//! ```
//!
//! Both nonces are random and fresh per connection. With `psk` the pre-shared key:
//!
//! ```text
//! prk        = HKDF-Extract(salt = client_nonce || server_nonce, ikm = psk)        (SHA-256)
//! transcript = SHA-256("rohi-control-tcp v1 transcript" || ClientHello || server_nonce)
//! k_c2s, k_s2c       = HKDF-Expand(prk, "rohi-control-tcp v1 key c2s" / "... s2c", 32)
//! n_c2s, n_s2c       = HKDF-Expand(prk, "rohi-control-tcp v1 nonce c2s" / "... s2c", 12)
//! server_proof       = HKDF-Expand(prk, "rohi-control-tcp v1 server proof" || transcript, 32)
//! client_proof       = HKDF-Expand(prk, "rohi-control-tcp v1 client proof" || transcript || server_proof, 32)
//! ```
//!
//! The server proves it knows the key before it receives anything else, the client proof is
//! bound to the server proof and to the whole transcript, so both sides are authenticated and the
//! nonces of a connection cannot be replayed into another one. A failed proof closes the
//! connection; nothing is processed before the client proof verified.
//!
//! A record is `version(1) | length u16 BE | ciphertext | tag(16)` where `length` counts the
//! ciphertext only. The AEAD is XChaCha20-Poly1305 with the key of the direction, the header as
//! associated data and a 24-byte nonce `prefix(12) || counter u64 BE || direction u32 BE` (1 for
//! client to device, 2 for device to client). The counter is not sent: each side counts the
//! records it sent and received, starting at 0, so a replayed, dropped, reordered or truncated
//! record does not authenticate. A side that would use the counter `u64::MAX` refuses to send or
//! receive and the connection ends: the counter is never reused.
//!
//! The key is never sent, never logged and the keys derived from it are wiped when the channel
//! is dropped. There is no forward secrecy: whoever learns the pre-shared key can decrypt a
//! recorded session.
//!
//! ```
//! use rohi_control_tcp::secure::{ClientHandshake, ServerHandshake, TAG_LEN};
//!
//! let psk = [7u8; 32];
//! let (client, hello) = ClientHandshake::start(&psk, &[1; 32]);
//! let (server, server_hello) = ServerHandshake::respond(&psk, &hello, &[2; 32]).unwrap();
//! let (mut client_channel, finish) = client.on_server_hello(&server_hello).unwrap();
//! let mut server_channel = server.finish(&finish).unwrap();
//!
//! // The client seals a request; the device opens it in place.
//! let mut record = *b"a request";
//! let (header, tag) = client_channel.seal(&mut record).unwrap();
//! assert_ne!(&record, b"a request");
//! let len = server_channel.open_header(header, 64).unwrap();
//! assert_eq!(len, record.len());
//! server_channel.open(header, &mut record, &tag).unwrap();
//! assert_eq!(&record, b"a request");
//! assert_eq!(TAG_LEN, 16);
//!
//! // A different key does not even complete the handshake.
//! let (other, hello) = ClientHandshake::start(&[8; 32], &[1; 32]);
//! let (_, server_hello) = ServerHandshake::respond(&psk, &hello, &[2; 32]).unwrap();
//! assert!(other.on_server_hello(&server_hello).is_err());
//! ```

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// Version of the handshake and of the record layer.
pub const VERSION: u8 = 1;
/// The first bytes of both hellos.
pub const MAGIC: [u8; 4] = *b"ROHI";
/// Size of the pre-shared key, in bytes.
pub const PSK_LEN: usize = 32;
/// Size of a handshake nonce, in bytes.
pub const NONCE_LEN: usize = 32;
/// Size of a proof, in bytes.
pub const PROOF_LEN: usize = 32;
/// Size of the `ClientHello`.
pub const CLIENT_HELLO_LEN: usize = 4 + 1 + NONCE_LEN;
/// Size of the `ServerHello`.
pub const SERVER_HELLO_LEN: usize = 4 + 1 + NONCE_LEN + PROOF_LEN;
/// Size of the `ClientFinish`.
pub const CLIENT_FINISH_LEN: usize = PROOF_LEN;
/// Size of the record header: version and the ciphertext length.
pub const RECORD_HEADER_LEN: usize = 3;
/// Size of the authentication tag that follows every record.
pub const TAG_LEN: usize = 16;

const KEY_LEN: usize = 32;
const NONCE_PREFIX_LEN: usize = 12;
const DIRECTION_C2S: u32 = 1;
const DIRECTION_S2C: u32 = 2;

/// Why a handshake did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandshakeError {
    /// The hello does not start with the magic bytes: not this protocol.
    Magic,
    /// The peer speaks another version.
    Version,
    /// The proof of the peer is wrong: another key, a replay or tampering.
    Proof,
}

/// Why a record was refused. Every one of them ends the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    /// The header names another version.
    Version,
    /// The record carries no data or more than the direction allows.
    Length,
    /// The record did not authenticate: tampered, replayed, reordered, dropped before it or
    /// from another connection.
    Authentication,
    /// The counter of this direction is exhausted: no record is accepted or sent any more.
    Exhausted,
}

/// A pre-shared key in memory: wiped when dropped.
struct Secret<const N: usize>([u8; N]);

impl<const N: usize> Drop for Secret<N> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

fn equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// What both sides derive from the key and the nonces.
struct Schedule {
    c2s: Secret<KEY_LEN>,
    s2c: Secret<KEY_LEN>,
    c2s_prefix: [u8; NONCE_PREFIX_LEN],
    s2c_prefix: [u8; NONCE_PREFIX_LEN],
    server_proof: [u8; PROOF_LEN],
    client_proof: [u8; PROOF_LEN],
}

impl Schedule {
    fn derive(
        psk: &[u8; PSK_LEN],
        hello: &[u8; CLIENT_HELLO_LEN],
        server_nonce: &[u8; NONCE_LEN],
    ) -> Self {
        let mut salt = [0u8; 2 * NONCE_LEN];
        salt[..NONCE_LEN].copy_from_slice(&hello[CLIENT_HELLO_LEN - NONCE_LEN..]);
        salt[NONCE_LEN..].copy_from_slice(server_nonce);
        let hkdf = Hkdf::<Sha256>::new(Some(&salt), psk);
        let mut transcript = Sha256::new();
        transcript.update(b"rohi-control-tcp v1 transcript");
        transcript.update(hello);
        transcript.update(server_nonce);
        let transcript: [u8; 32] = transcript.finalize().into();

        let mut schedule = Self {
            c2s: Secret([0; KEY_LEN]),
            s2c: Secret([0; KEY_LEN]),
            c2s_prefix: [0; NONCE_PREFIX_LEN],
            s2c_prefix: [0; NONCE_PREFIX_LEN],
            server_proof: [0; PROOF_LEN],
            client_proof: [0; PROOF_LEN],
        };
        // The lengths are far below the limit of HKDF-SHA256, so these cannot fail.
        let _ = hkdf.expand(b"rohi-control-tcp v1 key c2s", &mut schedule.c2s.0);
        let _ = hkdf.expand(b"rohi-control-tcp v1 key s2c", &mut schedule.s2c.0);
        let _ = hkdf.expand(b"rohi-control-tcp v1 nonce c2s", &mut schedule.c2s_prefix);
        let _ = hkdf.expand(b"rohi-control-tcp v1 nonce s2c", &mut schedule.s2c_prefix);
        let _ = hkdf.expand_multi_info(
            &[b"rohi-control-tcp v1 server proof", &transcript],
            &mut schedule.server_proof,
        );
        let _ = hkdf.expand_multi_info(
            &[
                b"rohi-control-tcp v1 client proof",
                &transcript,
                &schedule.server_proof,
            ],
            &mut schedule.client_proof,
        );
        salt.zeroize();
        schedule
    }

    // The keys are copied once into the channel; the schedule wipes its own on drop.
    fn channel(self, server: bool) -> Channel {
        let (tx_key, tx_prefix, rx_key, rx_prefix) = if server {
            (&self.s2c.0, self.s2c_prefix, &self.c2s.0, self.c2s_prefix)
        } else {
            (&self.c2s.0, self.c2s_prefix, &self.s2c.0, self.s2c_prefix)
        };
        let (tx_direction, rx_direction) = if server {
            (DIRECTION_S2C, DIRECTION_C2S)
        } else {
            (DIRECTION_C2S, DIRECTION_S2C)
        };
        Channel {
            tx: Half::new(tx_key, tx_prefix, tx_direction),
            rx: Half::new(rx_key, rx_prefix, rx_direction),
        }
    }
}

/// The client side of the handshake, between its hello and the answer of the device.
pub struct ClientHandshake {
    hello: [u8; CLIENT_HELLO_LEN],
    psk: Secret<PSK_LEN>,
}

impl ClientHandshake {
    /// Starts a handshake with `psk` and a fresh random `nonce`; returns the bytes to send.
    pub fn start(psk: &[u8; PSK_LEN], nonce: &[u8; NONCE_LEN]) -> (Self, [u8; CLIENT_HELLO_LEN]) {
        let mut hello = [0u8; CLIENT_HELLO_LEN];
        hello[..4].copy_from_slice(&MAGIC);
        hello[4] = VERSION;
        hello[5..].copy_from_slice(nonce);
        (
            Self {
                hello,
                psk: Secret(*psk),
            },
            hello,
        )
    }

    /// Checks the proof of the device and returns the channel and the final message to send.
    ///
    /// # Errors
    ///
    /// [`HandshakeError`]: not the protocol, another version, or a device that does not know the
    /// key.
    pub fn on_server_hello(
        self,
        server_hello: &[u8; SERVER_HELLO_LEN],
    ) -> Result<(Channel, [u8; CLIENT_FINISH_LEN]), HandshakeError> {
        if server_hello[..4] != MAGIC {
            return Err(HandshakeError::Magic);
        }
        if server_hello[4] != VERSION {
            return Err(HandshakeError::Version);
        }
        let mut server_nonce = [0u8; NONCE_LEN];
        server_nonce.copy_from_slice(&server_hello[5..5 + NONCE_LEN]);
        let schedule = Schedule::derive(&self.psk.0, &self.hello, &server_nonce);
        if !equal(&schedule.server_proof, &server_hello[5 + NONCE_LEN..]) {
            return Err(HandshakeError::Proof);
        }
        let finish = schedule.client_proof;
        Ok((schedule.channel(false), finish))
    }
}

/// The device side of the handshake, between its hello and the final message of the client.
pub struct ServerHandshake {
    schedule: Schedule,
}

impl ServerHandshake {
    /// Answers a `ClientHello` with a fresh random `server_nonce`; returns the bytes to send.
    ///
    /// # Errors
    ///
    /// [`HandshakeError::Magic`] or [`HandshakeError::Version`] for a hello of another protocol.
    pub fn respond(
        psk: &[u8; PSK_LEN],
        client_hello: &[u8; CLIENT_HELLO_LEN],
        server_nonce: &[u8; NONCE_LEN],
    ) -> Result<(Self, [u8; SERVER_HELLO_LEN]), HandshakeError> {
        if client_hello[..4] != MAGIC {
            return Err(HandshakeError::Magic);
        }
        if client_hello[4] != VERSION {
            return Err(HandshakeError::Version);
        }
        let schedule = Schedule::derive(psk, client_hello, server_nonce);
        let mut hello = [0u8; SERVER_HELLO_LEN];
        hello[..4].copy_from_slice(&MAGIC);
        hello[4] = VERSION;
        hello[5..5 + NONCE_LEN].copy_from_slice(server_nonce);
        hello[5 + NONCE_LEN..].copy_from_slice(&schedule.server_proof);
        Ok((Self { schedule }, hello))
    }

    /// Checks the proof of the client. Only a channel that comes out of this carries requests.
    ///
    /// # Errors
    ///
    /// [`HandshakeError::Proof`] for a client that does not know the key.
    pub fn finish(
        self,
        client_finish: &[u8; CLIENT_FINISH_LEN],
    ) -> Result<Channel, HandshakeError> {
        if !equal(&self.schedule.client_proof, client_finish) {
            return Err(HandshakeError::Proof);
        }
        Ok(self.schedule.channel(true))
    }
}

// One direction of a channel.
struct Half {
    key: Secret<KEY_LEN>,
    prefix: [u8; NONCE_PREFIX_LEN],
    direction: u32,
    counter: u64,
    exhausted: bool,
}

impl Half {
    const fn new(key: &[u8; KEY_LEN], prefix: [u8; NONCE_PREFIX_LEN], direction: u32) -> Self {
        Self {
            key: Secret(*key),
            prefix,
            direction,
            counter: 0,
            exhausted: false,
        }
    }

    // The nonce of the next record, and the counter moved on. `u64::MAX` is never used: the
    // channel stops one record earlier and stays stopped.
    fn next_nonce(&mut self) -> Result<XNonce, RecordError> {
        if self.exhausted || self.counter == u64::MAX {
            self.exhausted = true;
            return Err(RecordError::Exhausted);
        }
        let mut nonce = [0u8; 24];
        nonce[..NONCE_PREFIX_LEN].copy_from_slice(&self.prefix);
        nonce[NONCE_PREFIX_LEN..NONCE_PREFIX_LEN + 8].copy_from_slice(&self.counter.to_be_bytes());
        nonce[NONCE_PREFIX_LEN + 8..].copy_from_slice(&self.direction.to_be_bytes());
        self.counter += 1;
        Ok(XNonce::from(nonce))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&self.key.0.into())
    }
}

/// An established channel: the record layer of one connection.
pub struct Channel {
    tx: Half,
    rx: Half,
}

fn header(len: usize) -> [u8; RECORD_HEADER_LEN] {
    let len = (len as u16).to_be_bytes();
    [VERSION, len[0], len[1]]
}

impl Channel {
    /// Encrypts `buffer` in place as the next record to send and returns its header and tag: the
    /// record is `header`, then `buffer`, then `tag`.
    ///
    /// # Errors
    ///
    /// [`RecordError::Length`] for an empty buffer or one over 65 535 bytes and
    /// [`RecordError::Exhausted`] when the counter is used up.
    pub fn seal(
        &mut self,
        buffer: &mut [u8],
    ) -> Result<([u8; RECORD_HEADER_LEN], [u8; TAG_LEN]), RecordError> {
        if buffer.is_empty() || buffer.len() > usize::from(u16::MAX) {
            return Err(RecordError::Length);
        }
        let header = header(buffer.len());
        let nonce = self.tx.next_nonce()?;
        let tag = self
            .tx
            .cipher()
            .encrypt_in_place_detached(&nonce, &header, buffer)
            .map_err(|_| RecordError::Authentication)?;
        Ok((header, tag.into()))
    }

    /// The length of the data of a record with `header`, checked against `max`.
    ///
    /// # Errors
    ///
    /// [`RecordError::Version`] and [`RecordError::Length`]; the connection must end, the stream
    /// cannot be resynchronized.
    pub fn open_header(
        &self,
        header: [u8; RECORD_HEADER_LEN],
        max: usize,
    ) -> Result<usize, RecordError> {
        if header[0] != VERSION {
            return Err(RecordError::Version);
        }
        let len = usize::from(u16::from_be_bytes([header[1], header[2]]));
        if len == 0 || len > max {
            return Err(RecordError::Length);
        }
        Ok(len)
    }

    /// Decrypts `buffer`, the data of the record with `header` and `tag`, in place.
    ///
    /// # Errors
    ///
    /// [`RecordError::Authentication`] when the record is not the next one of the peer, intact;
    /// the buffer then holds garbage and must be dropped. [`RecordError::Exhausted`] when the
    /// counter is used up.
    pub fn open(
        &mut self,
        header: [u8; RECORD_HEADER_LEN],
        buffer: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), RecordError> {
        if header != self::header(buffer.len()) {
            return Err(RecordError::Authentication);
        }
        let nonce = self.rx.next_nonce()?;
        self.rx
            .cipher()
            .decrypt_in_place_detached(&nonce, &header, buffer, &Tag::from(*tag))
            .map_err(|_| RecordError::Authentication)
    }

    #[cfg(test)]
    pub(crate) fn set_counters(&mut self, tx: u64, rx: u64) {
        self.tx.counter = tx;
        self.rx.counter = rx;
    }
}

#[cfg(test)]
mod tests;
